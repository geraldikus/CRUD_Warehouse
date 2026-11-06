use uuid::Uuid;
use warehouse_core::models::{CreateItem, UpdateItem};

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
}

/// What the background thread has finished. The window drains this with
/// `try_recv`, so a slow request never freezes a frame.
#[derive(Debug)]
pub(crate) enum Event {
    Items(Vec<warehouse_core::models::Item>),
    Error(String),
    Remote(RemoteEvent),
    Mqtt(bool),
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
    report(&api, &events, &ctx, Command::Refresh).await;
    while let Some(command) = commands.recv().await {
        report(&api, &events, &ctx, command).await;
    }
}

async fn report(
    api: &ApiClient,
    events: &std::sync::mpsc::Sender<Event>,
    ctx: &egui::Context,
    command: Command,
) {
    let event = match perform(api, command).await {
        Ok(items) => Event::Items(items),
        Err(err) => {
            tracing::warn!(error = %err, "request failed");
            Event::Error(err.to_string())
        }
    };
    let _ = events.send(event);
    ctx.request_repaint();
}

async fn perform(
    api: &ApiClient,
    command: Command,
) -> Result<Vec<warehouse_core::models::Item>, crate::api::ApiError> {
    match command {
        Command::Refresh => api.list().await,
        Command::Create(input) => {
            api.create(&input).await?;
            api.list().await
        }
        Command::Update(id, input) => {
            api.update(id, &input).await?;
            api.list().await
        }
        Command::Delete(id) => {
            api.delete(id).await?;
            api.list().await
        }
    }
}
