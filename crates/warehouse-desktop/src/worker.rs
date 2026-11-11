use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use uuid::Uuid;
use warehouse_core::models::{
    AssistantReply, AssistantRequest, ChatMessage, CreateItem, UpdateItem,
};

use crate::api::ApiClient;
use crate::config::Config;
use crate::mqtt::{self, MqttUpdate, RemoteEvent};

/// Work the window asks the background thread to do.
#[derive(Debug)]
pub(crate) enum Command {
    Refresh,
    Create(CreateItem),
    Update(Uuid, UpdateItem),
    Delete(Uuid),
    /// The visible transcript, including the line the user just sent.
    Ask(Vec<ChatMessage>),
    /// Open the microphone and the speech websocket.
    VoiceStart,
    /// Close them. In-flight audio is dropped.
    VoiceStop,
    /// Synthesize `text` and play it. The microphone is muted for the duration.
    Speak(String),
}

/// What the background thread has finished. The window drains this with
/// `try_recv`, so a slow request never freezes a frame.
#[derive(Debug)]
pub(crate) enum Event {
    Items(Vec<warehouse_core::models::Item>),
    Error(String),
    /// The assistant request failed. This is separate from [`Event::Error`]
    /// so a later list refresh cannot clear the "thinking" flag by accident.
    AssistantError(String),
    AssistantReply(AssistantReply),
    Remote(RemoteEvent),
    Mqtt(bool),
    /// `true` while the microphone session is up.
    VoiceListening(bool),
    /// `true` while a reply is being spoken.
    VoiceSpeaking(bool),
    VoiceTranscript(String),
    VoiceError(String),
}

struct VoiceControl {
    stop: tokio::sync::watch::Sender<bool>,
    muted: Arc<AtomicBool>,
    done: tokio::sync::oneshot::Receiver<()>,
}

/// Start the tokio runtime on its own thread and return the two channels.
///
/// The UI thread owns the sender of commands and the receiver of events.
/// `ctx.request_repaint` wakes egui after a message arrives, because a
/// channel send does not by itself schedule a frame.
pub(crate) fn spawn(
    ctx: egui::Context,
    config: Config,
) -> (
    tokio::sync::mpsc::UnboundedSender<Command>,
    std::sync::mpsc::Receiver<Event>,
) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();

    let mqtt_commands = cmd_tx.clone();
    let mqtt_events = ev_tx.clone();
    let mqtt_ctx = ctx.clone();
    let mqtt_config = config.clone();

    std::thread::Builder::new()
        .name("warehouse-desktop-net".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            runtime.block_on(async move {
                tokio::spawn(async move {
                    mqtt::run(mqtt_config, move |update| {
                        match update {
                            MqttUpdate::Online(online) => {
                                let _ = mqtt_events.send(Event::Mqtt(online));
                            }
                            MqttUpdate::Message(remote) => {
                                if matches!(remote, RemoteEvent::Changed) {
                                    let _ = mqtt_commands.send(Command::Refresh);
                                }
                                let _ = mqtt_events.send(Event::Remote(remote));
                            }
                        }
                        mqtt_ctx.request_repaint();
                    })
                    .await;
                });

                run_commands(ApiClient::new(&config.api_url), cmd_rx, ev_tx, ctx).await;
            });
        })
        .expect("network thread");

    (cmd_tx, ev_rx)
}

async fn run_commands(
    api: ApiClient,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    events: std::sync::mpsc::Sender<Event>,
    ctx: egui::Context,
) {
    let listening = AtomicBool::new(false);
    let mut session: Option<VoiceControl> = None;

    report(&api, &events, &ctx, Command::Refresh, &listening).await;

    loop {
        let command = if let Some(control) = session.as_mut() {
            tokio::select! {
                biased;
                command = commands.recv() => command,
                _ = &mut control.done => {
                    session = None;
                    listening.store(false, Ordering::SeqCst);
                    continue;
                }
            }
        } else {
            commands.recv().await
        };
        let Some(command) = command else {
            break;
        };

        // A spoken reply is a `Speak` command queued by `report`, so it
        // takes the same path as an explicit speak request.
        let mut next = Some(command);
        while let Some(current) = next.take() {
            next = match current {
                Command::VoiceStart => {
                    if session.is_none() {
                        match start_voice(&api, &events, &ctx) {
                            Ok(control) => {
                                listening.store(true, Ordering::SeqCst);
                                session = Some(control);
                                let _ = events.send(Event::VoiceListening(true));
                                ctx.request_repaint();
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "microphone unavailable");
                                let _ = events.send(Event::VoiceError(err));
                                ctx.request_repaint();
                            }
                        }
                    }
                    None
                }
                Command::VoiceStop => {
                    // Stop speaking the reply that is still in flight. The
                    // button stays on Stop until the capture task actually ends.
                    listening.store(false, Ordering::SeqCst);
                    if let Some(control) = session.as_ref() {
                        let _ = control.stop.send(true);
                    }
                    None
                }
                Command::Speak(text) => {
                    let muted = session.as_ref().map(|control| Arc::clone(&control.muted));
                    speak(&api, &text, muted, &events, &ctx).await;
                    None
                }
                other => report(&api, &events, &ctx, other, &listening)
                    .await
                    .map(Command::Speak),
            };
        }
    }
}

fn start_voice(
    api: &ApiClient,
    events: &std::sync::mpsc::Sender<Event>,
    ctx: &egui::Context,
) -> Result<VoiceControl, String> {
    let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
    let muted = Arc::new(AtomicBool::new(false));
    // cpal's stream is `!Send`, so it has to be created on the thread that
    // owns it. The channel closes when the async session ends, and that
    // drops the device.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (keep_tx, keep_rx) = std::sync::mpsc::channel::<()>();
    let capture_tx = audio_tx;
    let capture_muted = Arc::clone(&muted);
    std::thread::Builder::new()
        .name("warehouse-mic".to_string())
        .spawn(move || {
            let mic = match crate::audio::start_capture(capture_tx, capture_muted) {
                Ok(mic) => mic,
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            let _mic = mic;
            let _ = keep_rx.recv();
        })
        .map_err(|err| err.to_string())?;
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err),
        Err(_) => return Err("microphone did not open".to_string()),
    }
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let url = api.voice_stream_url();
    let events = events.clone();
    let ctx = ctx.clone();
    let session_muted = Arc::clone(&muted);
    tokio::spawn(async move {
        let _keep_mic = keep_tx;
        if let Err(err) =
            crate::voice::run_session(url, audio_rx, stop_rx, session_muted, &events, &ctx).await
        {
            tracing::warn!(error = %err, "voice session ended");
            let _ = events.send(Event::VoiceError(err));
        }
        let _ = events.send(Event::VoiceListening(false));
        ctx.request_repaint();
        let _ = done_tx.send(());
    });
    Ok(VoiceControl {
        stop: stop_tx,
        muted,
        done: done_rx,
    })
}

async fn speak(
    api: &ApiClient,
    text: &str,
    muted: Option<Arc<AtomicBool>>,
    events: &std::sync::mpsc::Sender<Event>,
    ctx: &egui::Context,
) {
    let text = crate::speech::for_speech(text);
    if text.is_empty() {
        return;
    }
    if let Some(flag) = &muted {
        flag.store(true, Ordering::SeqCst);
    }
    let _ = events.send(Event::VoiceSpeaking(true));
    ctx.request_repaint();

    match api.tts(&text).await {
        Ok(wav) => {
            let played = tokio::task::spawn_blocking(move || crate::audio::play_wav(&wav)).await;
            if let Err(err) = flatten_play(played) {
                tracing::warn!(error = %err, "playback failed");
                let _ = events.send(Event::VoiceError(err));
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "tts failed");
            let _ = events.send(Event::VoiceError(err.to_string()));
        }
    }

    if let Some(flag) = &muted {
        flag.store(false, Ordering::SeqCst);
    }
    let _ = events.send(Event::VoiceSpeaking(false));
    ctx.request_repaint();
}

fn flatten_play(result: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), String> {
    match result {
        Ok(inner) => inner,
        Err(err) => Err(err.to_string()),
    }
}

async fn report(
    api: &ApiClient,
    events: &std::sync::mpsc::Sender<Event>,
    ctx: &egui::Context,
    command: Command,
    listening: &AtomicBool,
) -> Option<String> {
    let from_assistant = matches!(&command, Command::Ask(_));
    let spoken = match perform(api, command).await {
        Ok(Event::AssistantReply(reply)) => {
            let changed = reply.changed;
            let spoken = reply.reply.clone();
            let _ = events.send(Event::AssistantReply(reply));
            // MQTT may be offline. The table still has to show what the
            // assistant just wrote.
            if changed {
                match api.list().await {
                    Ok(items) => {
                        let _ = events.send(Event::Items(items));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "refresh after assistant failed");
                        let _ = events.send(Event::Error(err.to_string()));
                    }
                }
            }
            if listening.load(Ordering::SeqCst) {
                Some(spoken)
            } else {
                None
            }
        }
        Ok(event) => {
            let _ = events.send(event);
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "request failed");
            let event = if from_assistant {
                Event::AssistantError(err.to_string())
            } else {
                Event::Error(err.to_string())
            };
            let _ = events.send(event);
            None
        }
    };
    ctx.request_repaint();
    spoken
}

async fn perform(api: &ApiClient, command: Command) -> Result<Event, crate::api::ApiError> {
    match command {
        Command::Refresh => Ok(Event::Items(api.list().await?)),
        Command::Create(input) => {
            api.create(&input).await?;
            Ok(Event::Items(api.list().await?))
        }
        Command::Update(id, input) => {
            api.update(id, &input).await?;
            Ok(Event::Items(api.list().await?))
        }
        Command::Delete(id) => {
            api.delete(id).await?;
            Ok(Event::Items(api.list().await?))
        }
        Command::Ask(messages) => {
            let reply = api.ask(&AssistantRequest { messages }).await?;
            Ok(Event::AssistantReply(reply))
        }
        Command::VoiceStart | Command::VoiceStop | Command::Speak(_) => {
            unreachable!("voice commands are handled before perform")
        }
    }
}
