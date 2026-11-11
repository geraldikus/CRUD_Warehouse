use chrono::{DateTime, Utc};
use jiff::civil::Date;
use uuid::Uuid;
use warehouse_core::models::{
    AssistantReply, ChatMessage, ChatRole, CreateItem, Item, PendingAdd, PendingAddMatch,
    PendingDelete, UpdateItem,
};
use warehouse_core::validation::validate_item;

use crate::expiry::{civil_date_of, utc_end_of_civil_day};
use crate::mqtt::RemoteEvent;
use crate::theme;
use crate::worker::{Command, Event};

const MAX_NOTICES: usize = 8;

/// State of the window. Widgets only read and write these fields.
pub struct WarehouseApp {
    items: Vec<Item>,
    error: Option<String>,
    notices: Vec<String>,
    mqtt_online: bool,
    form_name: String,
    form_quantity: i32,
    form_has_expiry: bool,
    form_expiry: Date,
    editing: Option<Uuid>,
    chat: Vec<ChatMessage>,
    chat_input: String,
    chat_busy: bool,
    pending_delete: Option<PendingDelete>,
    pending_add: Option<PendingAdd>,
    listening: bool,
    speaking: bool,
    now: DateTime<Utc>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    events: std::sync::mpsc::Receiver<Event>,
}

impl WarehouseApp {
    pub fn new(ctx: egui::Context, config: crate::config::Config) -> Self {
        theme::apply(&ctx);
        let (commands, events) = crate::worker::spawn(ctx, config);
        Self::from_channels(commands, events)
    }

    fn from_channels(
        commands: tokio::sync::mpsc::UnboundedSender<Command>,
        events: std::sync::mpsc::Receiver<Event>,
    ) -> Self {
        Self {
            items: Vec::new(),
            error: None,
            notices: Vec::new(),
            mqtt_online: false,
            form_name: String::new(),
            form_quantity: 1,
            form_has_expiry: false,
            form_expiry: today(),
            editing: None,
            chat: Vec::new(),
            chat_input: String::new(),
            chat_busy: false,
            pending_delete: None,
            pending_add: None,
            listening: false,
            speaking: false,
            now: Utc::now(),
            commands,
            events,
        }
    }

    /// Pull whatever the background thread has finished. Never blocks.
    fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.apply_event(event);
        }
    }

    pub(crate) fn apply_event(&mut self, event: Event) {
        match event {
            Event::Items(items) => self.items = items,
            Event::Error(message) => self.error = Some(message),
            Event::AssistantError(message) => {
                self.chat_busy = false;
                self.error = Some(message);
            }
            Event::AssistantReply(reply) => self.apply_reply(reply),
            Event::VoiceListening(on) => self.listening = on,
            Event::VoiceSpeaking(on) => self.speaking = on,
            Event::VoiceTranscript(text) => self.accept_transcript(text),
            Event::VoiceError(message) => self.error = Some(message),
            Event::Mqtt(online) => self.mqtt_online = online,
            Event::Remote(RemoteEvent::Changed) => {}
            Event::Remote(RemoteEvent::Expiring { name, hours_left }) => {
                self.push_notice(format!("{name}: expires in under {hours_left}h"));
            }
            Event::Remote(RemoteEvent::Expired { name }) => {
                self.push_notice(format!("{name}: expired"));
            }
        }
    }

    fn apply_reply(&mut self, reply: AssistantReply) {
        self.chat_busy = false;
        self.pending_delete = reply.pending_delete;
        self.pending_add = reply.pending_add;
        self.chat.push(ChatMessage {
            role: ChatRole::Assistant,
            text: reply.reply,
        });
    }

    fn add_to_existing(&mut self, existing: &PendingAddMatch) {
        let Some(pending) = &self.pending_add else {
            return;
        };
        let _ = self.commands.send(Command::Update(
            existing.id,
            UpdateItem {
                name: existing.name.clone(),
                quantity: existing.quantity + pending.quantity,
                expires_at: existing.expires_at,
            },
        ));
        self.pending_add = None;
    }

    fn create_from_pending(&mut self) {
        let Some(pending) = self.pending_add.take() else {
            return;
        };
        let _ = self.commands.send(Command::Create(CreateItem {
            name: pending.name,
            quantity: pending.quantity,
            expires_at: pending.expires_at,
        }));
    }

    /// The worker sends the whole transcript. Tool calls stay on the server.
    fn send_chat(&mut self) {
        if self.chat_busy {
            return;
        }
        let text = self.chat_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.chat.push(ChatMessage {
            role: ChatRole::User,
            text,
        });
        self.chat_input.clear();
        self.chat_busy = true;
        self.error = None;
        let _ = self.commands.send(Command::Ask(self.chat.clone()));
    }

    /// A phrase from the recognizer takes the same path as the Send button.
    /// Delete and add still wait for the existing confirm buttons.
    fn accept_transcript(&mut self, text: String) {
        if self.chat_busy {
            return;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.chat.push(ChatMessage {
            role: ChatRole::User,
            text,
        });
        self.chat_busy = true;
        self.error = None;
        let _ = self.commands.send(Command::Ask(self.chat.clone()));
    }

    fn push_notice(&mut self, text: String) {
        self.notices.push(text);
        let overflow = self.notices.len().saturating_sub(MAX_NOTICES);
        if overflow > 0 {
            self.notices.drain(0..overflow);
        }
    }

    /// Local check, then a command. The worker talks to the API.
    fn submit(&mut self) {
        let name = self.form_name.trim().to_string();
        if let Err(err) = validate_item(&name, self.form_quantity) {
            self.error = Some(err.to_string());
            return;
        }

        let expires_at = self
            .form_has_expiry
            .then(|| utc_end_of_civil_day(self.form_expiry));
        let sent = if let Some(id) = self.editing {
            self.commands.send(Command::Update(
                id,
                UpdateItem {
                    name,
                    quantity: self.form_quantity,
                    expires_at,
                },
            ))
        } else {
            self.commands.send(Command::Create(CreateItem {
                name,
                quantity: self.form_quantity,
                expires_at,
            }))
        };

        if sent.is_ok() {
            self.error = None;
            self.form_name.clear();
            self.editing = None;
            self.form_has_expiry = false;
        }
    }

    fn begin_edit(&mut self, item: &Item) {
        self.editing = Some(item.id);
        self.form_name = item.name.clone();
        self.form_quantity = item.quantity;
        self.form_has_expiry = item.expires_at.is_some();
        if let Some(expires_at) = item.expires_at {
            self.form_expiry = civil_date_of(expires_at);
        }
        self.error = None;
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Warehouse");
            ui.add_space(8.0);
            if ui.add(theme::accent_button("Refresh")).clicked() {
                let _ = self.commands.send(Command::Refresh);
            }
            let (text, color) = if self.mqtt_online {
                ("MQTT online", theme::OK)
            } else {
                ("MQTT offline", theme::MUTED)
            };
            chip(ui, text, color);
            ui.colored_label(theme::MUTED, format!("{} items", self.items.len()));
        });
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            if self.editing.is_some() {
                ui.colored_label(theme::ACCENT, "Editing");
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.form_name)
                        .hint_text("Name")
                        .desired_width(240.0),
                );
                ui.add(
                    egui::DragValue::new(&mut self.form_quantity)
                        .range(0..=i32::MAX)
                        .speed(1.0)
                        .prefix("Qty "),
                );
                ui.checkbox(&mut self.form_has_expiry, "Expiry");
                if self.form_has_expiry {
                    ui.add(egui_extras::DatePickerButton::new(&mut self.form_expiry));
                }
                let label = if self.editing.is_some() {
                    "Save"
                } else {
                    "Add"
                };
                if ui.add(theme::accent_button(label)).clicked() {
                    self.submit();
                }
                if self.editing.is_some() && ui.button("Cancel").clicked() {
                    self.editing = None;
                    self.form_name.clear();
                    self.form_has_expiry = false;
                }
            });
        });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        if self.items.is_empty() {
            ui.label("No items yet.");
            return;
        }

        let items = self.items.clone();
        let now = self.now;
        let mut edit = None;
        let mut delete = None;

        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .column(egui_extras::Column::remainder().at_least(140.0))
            .column(egui_extras::Column::exact(110.0))
            .column(egui_extras::Column::exact(180.0))
            .column(egui_extras::Column::exact(200.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Name");
                });
                header.col(|ui| {
                    ui.strong("Quantity");
                });
                header.col(|ui| {
                    ui.strong("Expires");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|mut body| {
                for item in &items {
                    let status = crate::expiry::expiry_status(item.expires_at, now);
                    body.row(32.0, |mut row| {
                        row.col(|ui| labeled(ui, &item.name, status));
                        row.col(|ui| labeled(ui, item.quantity.to_string(), status));
                        row.col(|ui| labeled(ui, format_expiry(item.expires_at), status));
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Edit").clicked() {
                                    edit = Some(item.clone());
                                }
                                if ui.add(theme::danger_button("Delete")).clicked() {
                                    delete = Some(item.id);
                                }
                            });
                        });
                    });
                }
            });

        if let Some(item) = edit {
            self.begin_edit(&item);
        }
        if let Some(id) = delete {
            let _ = self.commands.send(Command::Delete(id));
            if self.editing == Some(id) {
                self.editing = None;
                self.form_name.clear();
            }
        }
    }

    fn chat_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Assistant");
            if self.speaking {
                ui.colored_label(theme::WARN, "speaking");
            } else if self.listening {
                ui.colored_label(theme::OK, "listening");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.listening { "Stop" } else { "Listen" };
                if ui.add(theme::accent_button(label)).clicked() {
                    let command = if self.listening {
                        Command::VoiceStop
                    } else {
                        Command::VoiceStart
                    };
                    let _ = self.commands.send(command);
                }
            });
        });

        // Clone the id out of the option so the buttons can clear it.
        // The closure would otherwise borrow `self` twice.
        let pending = self.pending_delete.clone();
        if let Some(pending) = pending {
            egui::Frame::new()
                .fill(theme::PENDING_BG)
                .stroke(egui::Stroke::new(1.0, theme::PENDING_LINE))
                .corner_radius(12)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.label(format!("Delete {}?", pending.name));
                    ui.horizontal(|ui| {
                        if ui.add(theme::danger_button("Confirm delete")).clicked() {
                            let _ = self.commands.send(Command::Delete(pending.id));
                            self.pending_delete = None;
                        }
                        if ui.button("Keep item").clicked() {
                            self.pending_delete = None;
                        }
                    });
                });
        }

        let pending_add = self.pending_add.clone();
        if let Some(pending) = pending_add {
            egui::Frame::new()
                .fill(theme::CARD)
                .stroke(egui::Stroke::new(1.0, theme::LINE))
                .corner_radius(12)
                .inner_margin(10)
                .show(ui, |ui| {
                    if pending.matches.len() == 1 {
                        ui.label(format!(
                            "{} already has {}. Add {} to it, or create a new row?",
                            pending.matches[0].name, pending.matches[0].quantity, pending.quantity
                        ));
                    } else {
                        ui.label(format!(
                            "{} already exists. Add {} to an existing row, or create a new row?",
                            pending.name, pending.quantity
                        ));
                    }
                    ui.horizontal_wrapped(|ui| {
                        let mut chosen = None;
                        for existing in &pending.matches {
                            let label = add_to_existing_label(existing);
                            if ui.add(theme::accent_button(&label)).clicked() {
                                chosen = Some(existing.clone());
                            }
                        }
                        let create = ui.button("Create new").clicked();
                        let cancel = ui.button("Cancel").clicked();
                        if let Some(existing) = chosen {
                            self.add_to_existing(&existing);
                        } else if create {
                            self.create_from_pending();
                        } else if cancel {
                            self.pending_add = None;
                        }
                    });
                });
        }

        let scroll_height = (ui.available_height() - 48.0).max(80.0);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(scroll_height)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.chat.is_empty() && !self.chat_busy {
                    ui.colored_label(theme::MUTED, "Ask about stock, or tell me to add an item.");
                }
                for message in &self.chat {
                    chat_bubble(ui, message.role, &message.text);
                }
                if self.chat_busy {
                    thinking_indicator(ui);
                }
            });

        ui.horizontal(|ui| {
            let width = (ui.available_width() - 72.0).max(80.0);
            let editor = egui::TextEdit::singleline(&mut self.chat_input)
                .hint_text("Ask the warehouse")
                .desired_width(width);
            let response = ui.add_enabled(!self.chat_busy, editor);
            let submit =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let send = ui
                .add_enabled(!self.chat_busy, theme::accent_button("Send"))
                .clicked();
            if submit || send {
                self.send_chat();
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.colored_label(theme::DANGER, error);
        }
        for notice in &self.notices {
            ui.colored_label(theme::MUTED, notice);
        }
    }
}

impl eframe::App for WarehouseApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.now = Utc::now();
        if self.chat_busy {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Side panels take their space before the center is drawn.
        // The status panel is above the form in this function, so a click
        // in the form shows its error on the next frame. `logic` then
        // only drains the network channel.
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("assistant")
            .default_size(360.0)
            .min_size(240.0)
            .max_size(480.0)
            .show(ui, |ui| self.chat_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.form(ui);
            ui.add_space(8.0);
            self.table(ui);
        });
    }
}

fn today() -> Date {
    jiff::Zoned::now().date()
}

fn format_expiry(expires_at: Option<DateTime<Utc>>) -> String {
    match expires_at {
        Some(at) => at.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => "—".to_string(),
    }
}

fn add_to_existing_label(existing: &PendingAddMatch) -> String {
    format!("Add to existing ({})", existing.quantity)
}

fn labeled(ui: &mut egui::Ui, text: impl Into<String>, status: crate::expiry::ExpiryStatus) {
    let text = text.into();
    match status {
        crate::expiry::ExpiryStatus::Expired => {
            ui.colored_label(theme::DANGER, text);
        }
        crate::expiry::ExpiryStatus::Soon => {
            ui.colored_label(theme::WARN, text);
        }
        crate::expiry::ExpiryStatus::Ok | crate::expiry::ExpiryStatus::None => {
            ui.label(text);
        }
    }
}

fn chip(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.colored_label(color, text);
        });
}

fn chat_bubble(ui: &mut egui::Ui, role: ChatRole, text: &str) {
    let (who, fill, align_right) = match role {
        ChatRole::User => ("You", theme::BUBBLE_USER, true),
        ChatRole::Assistant => ("Assistant", theme::BUBBLE_ASSISTANT, false),
    };
    let layout = if align_right {
        egui::Layout::right_to_left(egui::Align::TOP)
    } else {
        egui::Layout::left_to_right(egui::Align::TOP)
    };
    ui.with_layout(layout, |ui| {
        let max_width = (ui.available_width() * 0.88).max(80.0);
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(12)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_max_width(max_width);
                ui.vertical(|ui| {
                    ui.colored_label(theme::MUTED, who);
                    ui.label(text);
                });
            });
    });
}

fn thinking_indicator(ui: &mut egui::Ui) {
    let time = ui.input(|input| input.time);
    let captions = ["Checking stock…", "Counting crates…", "Thinking…"];
    let caption = captions[((time / 1.5) as usize) % captions.len()];

    egui::Frame::new()
        .fill(theme::BUBBLE_ASSISTANT)
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .corner_radius(12)
        .inner_margin(10)
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(168.0, 36.0), egui::Sense::hover());
            let painter = ui.painter();
            for index in 0..3 {
                let hop = (((time * 3.4 + f64::from(index) * 0.7).sin() as f32) + 1.0) * 5.0;
                let center = egui::pos2(
                    rect.left() + 36.0 + index as f32 * 32.0,
                    rect.center().y + 6.0 - hop,
                );
                let crate_rect = egui::Rect::from_center_size(center, egui::vec2(20.0, 16.0));
                painter.rect_stroke(
                    crate_rect,
                    3.0,
                    egui::Stroke::new(2.0, theme::ACCENT),
                    egui::StrokeKind::Inside,
                );
                painter.hline(
                    crate_rect.x_range().shrink(3.0),
                    crate_rect.center().y,
                    egui::Stroke::new(1.5, theme::ACCENT),
                );
            }

            let scan_y = rect.bottom() - 4.0;
            painter.hline(
                rect.x_range().shrink(16.0),
                scan_y,
                egui::Stroke::new(3.0, theme::LINE),
            );
            let scan_width = 48.0;
            let travel = (rect.width() - 32.0 - scan_width).max(8.0);
            let x = rect.left() + 16.0 + ((time * 1.6).sin() as f32 * 0.5 + 0.5) * travel;
            painter.hline(
                x..=x + scan_width,
                scan_y,
                egui::Stroke::new(3.0, theme::ACCENT),
            );

            ui.add_space(4.0);
            ui.colored_label(theme::MUTED, caption);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt::RemoteEvent;
    use crate::worker::{Command, Event};
    use egui_kittest::kittest::Queryable;

    fn sample_item(name: &str) -> Item {
        serde_json::from_str(&format!(
            r#"{{
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "name": "{name}",
                "quantity": 4,
                "created_at": "2026-01-02T03:04:05Z",
                "updated_at": "2026-01-02T03:04:05Z",
                "expires_at": null
            }}"#
        ))
        .unwrap()
    }

    fn disconnected() -> (WarehouseApp, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let (commands, commands_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_events_tx, events_rx) = std::sync::mpsc::channel();
        (
            WarehouseApp::from_channels(commands, events_rx),
            commands_rx,
        )
    }

    #[test]
    fn items_event_replaces_the_list() {
        let (mut app, _rx) = disconnected();
        app.error = Some("old".to_string());
        app.apply_event(Event::Items(vec![sample_item("bolts")]));

        assert_eq!(app.items.len(), 1);
        assert_eq!(app.items[0].name, "bolts");
        // A list arriving from the network must not wipe a validation
        // message the user just produced locally.
        assert_eq!(app.error.as_deref(), Some("old"));
    }

    #[test]
    fn error_event_is_stored() {
        let (mut app, _rx) = disconnected();
        app.apply_event(Event::Error("database error".to_string()));
        assert_eq!(app.error.as_deref(), Some("database error"));
    }

    #[test]
    fn expiring_event_is_a_notice() {
        let (mut app, _rx) = disconnected();
        app.apply_event(Event::Remote(RemoteEvent::Expiring {
            name: "milk".to_string(),
            hours_left: 24,
        }));
        app.apply_event(Event::Remote(RemoteEvent::Expired {
            name: "milk".to_string(),
        }));

        assert_eq!(app.notices.len(), 2);
        assert!(app.notices[0].contains("milk"));
        assert!(app.notices[0].contains("24"));
        assert!(app.notices[1].contains("expired"));
    }

    #[test]
    fn notices_keep_only_the_latest() {
        let (mut app, _rx) = disconnected();
        for index in 0..10 {
            app.apply_event(Event::Remote(RemoteEvent::Expired {
                name: format!("item-{index}"),
            }));
        }
        assert_eq!(app.notices.len(), MAX_NOTICES);
        assert!(app.notices[0].contains("item-2"));
        assert!(app.notices[7].contains("item-9"));
    }

    #[test]
    fn changed_event_does_not_touch_the_list() {
        let (mut app, _rx) = disconnected();
        app.items = vec![sample_item("bolts")];
        app.apply_event(Event::Remote(RemoteEvent::Changed));
        assert_eq!(app.items.len(), 1);
    }

    #[test]
    fn mqtt_flag_follows_the_event() {
        let (mut app, _rx) = disconnected();
        app.apply_event(Event::Mqtt(true));
        assert!(app.mqtt_online);
        app.apply_event(Event::Mqtt(false));
        assert!(!app.mqtt_online);
    }

    #[test]
    fn empty_name_does_not_send_a_command() {
        let (mut app, mut commands) = disconnected();
        app.form_name.clear();
        app.submit();

        let message = app.error.expect("validation error");
        assert!(message.contains("name"), "{message}");
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn valid_create_sends_a_command() {
        let (mut app, mut commands) = disconnected();
        app.form_name = " bolts ".to_string();
        app.form_quantity = 3;
        app.submit();

        match commands.try_recv().unwrap() {
            Command::Create(input) => {
                assert_eq!(input.name, "bolts");
                assert_eq!(input.quantity, 3);
                assert_eq!(input.expires_at, None);
            }
            other => panic!("expected create, got {other:?}"),
        }
        assert!(app.error.is_none());
        assert!(app.form_name.is_empty());
    }

    #[test]
    fn add_button_with_empty_name_shows_validation_error() {
        let (app, mut commands) = disconnected();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1040.0, 680.0))
            .build_eframe(|_cc| app);

        harness.get_by_label("Add").click();
        harness.run();

        let message = harness.state().error.clone().expect("validation error");
        assert!(message.contains("name"), "{message}");
        assert!(commands.try_recv().is_err());

        // The status panel is drawn before the button in the same pass,
        // so the text appears on the following frame.
        harness.run();
        harness.get_by_label(&message);
    }

    #[test]
    fn send_chat_queues_the_transcript_and_ignores_a_second_send() {
        let (mut app, mut commands) = disconnected();
        app.chat_input = "  add bolts  ".to_string();
        app.send_chat();

        match commands.try_recv().unwrap() {
            Command::Ask(messages) => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, ChatRole::User);
                assert_eq!(messages[0].text, "add bolts");
            }
            other => panic!("expected ask, got {other:?}"),
        }
        assert!(app.chat_busy);
        assert!(app.chat_input.is_empty());

        app.chat_input = "again".to_string();
        app.send_chat();
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn blank_chat_input_does_not_send() {
        let (mut app, mut commands) = disconnected();
        app.chat_input = "   ".to_string();
        app.send_chat();
        assert!(commands.try_recv().is_err());
        assert!(app.chat.is_empty());
        assert!(!app.chat_busy);
    }

    #[test]
    fn assistant_reply_unlocks_input_and_stores_a_pending_delete() {
        let (mut app, _rx) = disconnected();
        app.chat_busy = true;
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        app.apply_event(Event::AssistantReply(AssistantReply {
            reply: "Press confirm.".to_string(),
            changed: true,
            pending_delete: Some(PendingDelete {
                id,
                name: "bolts".to_string(),
            }),
            pending_add: None,
        }));

        assert!(!app.chat_busy);
        assert_eq!(app.chat.len(), 1);
        assert_eq!(app.chat[0].role, ChatRole::Assistant);
        assert_eq!(app.chat[0].text, "Press confirm.");
        assert_eq!(app.pending_delete.unwrap().id, id);
    }

    #[test]
    fn assistant_error_unlocks_the_input() {
        let (mut app, _rx) = disconnected();
        app.chat_busy = true;
        app.apply_event(Event::AssistantError(
            "assistant request failed".to_string(),
        ));
        assert!(!app.chat_busy);
        assert_eq!(app.error.as_deref(), Some("assistant request failed"));
    }

    #[test]
    fn send_button_asks_the_assistant() {
        let (mut app, mut commands) = disconnected();
        app.chat_input = "add 3 bolts".to_string();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);

        harness.get_by_label("Send").click();
        // The thinking animation requests a repaint every frame, so run() never settles.
        harness.run_steps(2);

        match commands.try_recv().unwrap() {
            Command::Ask(messages) => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].text, "add 3 bolts");
            }
            other => panic!("expected ask, got {other:?}"),
        }
        assert!(harness.state().chat_busy);
    }

    #[test]
    fn listen_button_sends_voice_start() {
        let (app, mut commands) = disconnected();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);

        harness.get_by_label("Listen").click();
        harness.run();

        match commands.try_recv().unwrap() {
            Command::VoiceStart => {}
            other => panic!("expected voice start, got {other:?}"),
        }
    }

    #[test]
    fn transcript_event_asks_and_shows_the_phrase() {
        let (mut app, mut commands) = disconnected();
        app.apply_event(Event::VoiceTranscript("add 3 bolts".to_string()));

        match commands.try_recv().unwrap() {
            Command::Ask(messages) => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, ChatRole::User);
                assert_eq!(messages[0].text, "add 3 bolts");
            }
            other => panic!("expected ask, got {other:?}"),
        }
        assert!(app.chat_busy);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);
        // The thinking animation requests a repaint every frame, so run() never settles.
        harness.run_steps(2);
        harness.get_by_label("add 3 bolts");
    }

    #[test]
    fn assistant_reply_is_visible_in_the_panel() {
        let (mut app, _rx) = disconnected();
        app.apply_event(Event::AssistantReply(AssistantReply {
            reply: "Added bolts.".to_string(),
            changed: false,
            pending_delete: None,
            pending_add: None,
        }));
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);

        harness.run();
        harness.get_by_label("Added bolts.");
    }

    #[test]
    fn confirm_delete_sends_delete_and_keep_does_not() {
        let (mut app, mut commands) = disconnected();
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        app.pending_delete = Some(PendingDelete {
            id,
            name: "bolts".to_string(),
        });
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);

        harness.get_by_label("Confirm delete").click();
        harness.run();

        match commands.try_recv().unwrap() {
            Command::Delete(got) => assert_eq!(got, id),
            other => panic!("expected delete, got {other:?}"),
        }
        assert!(harness.state().pending_delete.is_none());

        harness.state_mut().pending_delete = Some(PendingDelete {
            id,
            name: "bolts".to_string(),
        });
        harness.run();
        harness.get_by_label("Keep item").click();
        harness.run();
        assert!(harness.state().pending_delete.is_none());
        assert!(commands.try_recv().is_err());
    }

    fn sample_pending_add() -> PendingAdd {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        PendingAdd {
            name: "Dish".to_string(),
            quantity: 2,
            expires_at: None,
            matches: vec![PendingAddMatch {
                id,
                name: "Dish".to_string(),
                quantity: 10,
                expires_at: None,
            }],
        }
    }

    #[test]
    fn assistant_reply_stores_a_pending_add() {
        let (mut app, _rx) = disconnected();
        let pending = sample_pending_add();
        app.apply_event(Event::AssistantReply(AssistantReply {
            reply: "Dish is already on a row.".to_string(),
            changed: false,
            pending_delete: None,
            pending_add: Some(pending.clone()),
        }));
        assert_eq!(app.pending_add, Some(pending));
    }

    #[test]
    fn pending_add_buttons_update_create_or_cancel() {
        let (mut app, mut commands) = disconnected();
        let pending = sample_pending_add();
        let id = pending.matches[0].id;
        app.pending_add = Some(pending);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);

        harness.get_by_label("Add to existing (10)").click();
        harness.run();

        match commands.try_recv().unwrap() {
            Command::Update(got, item) => {
                assert_eq!(got, id);
                assert_eq!(item.name, "Dish");
                assert_eq!(item.quantity, 12);
                assert_eq!(item.expires_at, None);
            }
            other => panic!("expected update, got {other:?}"),
        }
        assert!(harness.state().pending_add.is_none());

        harness.state_mut().pending_add = Some(sample_pending_add());
        harness.run();
        harness.get_by_label("Create new").click();
        harness.run();

        match commands.try_recv().unwrap() {
            Command::Create(item) => {
                assert_eq!(item.name, "Dish");
                assert_eq!(item.quantity, 2);
            }
            other => panic!("expected create, got {other:?}"),
        }
        assert!(harness.state().pending_add.is_none());

        harness.state_mut().pending_add = Some(sample_pending_add());
        harness.run();
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(harness.state().pending_add.is_none());
        assert!(commands.try_recv().is_err());
    }
}
