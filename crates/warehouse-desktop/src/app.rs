use chrono::{DateTime, Utc};
use jiff::civil::Date;
use uuid::Uuid;
use warehouse_core::models::{
    AssistantReply, ChatMessage, ChatRole, CreateItem, Item, PendingDelete, UpdateItem,
};
use warehouse_core::validation::validate_item;

use crate::expiry::{civil_date_of, utc_end_of_civil_day};
use crate::mqtt::RemoteEvent;
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
    now: DateTime<Utc>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    events: std::sync::mpsc::Receiver<Event>,
}

impl WarehouseApp {
    pub fn new(ctx: egui::Context, config: crate::config::Config) -> Self {
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
        self.chat.push(ChatMessage {
            role: ChatRole::Assistant,
            text: reply.reply,
        });
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
            if ui.button("Refresh").clicked() {
                let _ = self.commands.send(Command::Refresh);
            }
            let (text, color) = if self.mqtt_online {
                ("MQTT online", egui::Color32::from_rgb(30, 140, 60))
            } else {
                ("MQTT offline", egui::Color32::GRAY)
            };
            ui.colored_label(color, text);
            ui.label(format!("{} items", self.items.len()));
        });
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        if self.editing.is_some() {
            ui.label("Editing");
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
            if ui.button(label).clicked() {
                self.submit();
            }
            if self.editing.is_some() && ui.button("Cancel").clicked() {
                self.editing = None;
                self.form_name.clear();
                self.form_has_expiry = false;
            }
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
            .header(22.0, |mut header| {
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
                    body.row(28.0, |mut row| {
                        row.col(|ui| labeled(ui, &item.name, status));
                        row.col(|ui| labeled(ui, item.quantity.to_string(), status));
                        row.col(|ui| labeled(ui, format_expiry(item.expires_at), status));
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Edit").clicked() {
                                    edit = Some(item.clone());
                                }
                                if ui.button("Delete").clicked() {
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
        ui.heading("Assistant");
        if self.chat_busy {
            ui.label("Thinking...");
        }

        // Clone the id out of the option so the buttons can clear it.
        // The closure would otherwise borrow `self` twice.
        let pending = self.pending_delete.clone();
        if let Some(pending) = pending {
            ui.label(format!("Delete {}?", pending.name));
            ui.horizontal(|ui| {
                if ui.button("Confirm delete").clicked() {
                    let _ = self.commands.send(Command::Delete(pending.id));
                    self.pending_delete = None;
                }
                if ui.button("Keep item").clicked() {
                    self.pending_delete = None;
                }
            });
        }

        let scroll_height = (ui.available_height() - 40.0).max(80.0);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(scroll_height)
            .show(ui, |ui| {
                if self.chat.is_empty() && !self.chat_busy {
                    ui.label("Ask about stock, or tell me to add an item.");
                }
                for message in &self.chat {
                    let who = match message.role {
                        ChatRole::User => "You",
                        ChatRole::Assistant => "Assistant",
                    };
                    ui.label(format!("{who}: {}", message.text));
                }
            });

        ui.horizontal(|ui| {
            let width = (ui.available_width() - 64.0).max(80.0);
            let editor = egui::TextEdit::singleline(&mut self.chat_input)
                .hint_text("Ask the warehouse")
                .desired_width(width);
            let response = ui.add_enabled(!self.chat_busy, editor);
            let submit =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let send = ui
                .add_enabled(!self.chat_busy, egui::Button::new("Send"))
                .clicked();
            if submit || send {
                self.send_chat();
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(190, 50, 50), error);
        }
        for notice in &self.notices {
            ui.label(notice);
        }
    }
}

impl eframe::App for WarehouseApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.now = Utc::now();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Side panels take their space before the center is drawn.
        // The status panel is above the form in this function, so a click
        // in the form shows its error on the next frame. `logic` then
        // only drains the network channel.
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("assistant")
            .default_size(320.0)
            .min_size(240.0)
            .max_size(480.0)
            .show(ui, |ui| self.chat_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.form(ui);
            ui.separator();
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

fn labeled(ui: &mut egui::Ui, text: impl Into<String>, status: crate::expiry::ExpiryStatus) {
    let text = text.into();
    match status {
        crate::expiry::ExpiryStatus::Expired => {
            ui.colored_label(egui::Color32::from_rgb(190, 50, 50), text);
        }
        crate::expiry::ExpiryStatus::Soon => {
            ui.colored_label(egui::Color32::from_rgb(180, 130, 20), text);
        }
        crate::expiry::ExpiryStatus::Ok | crate::expiry::ExpiryStatus::None => {
            ui.label(text);
        }
    }
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
        harness.run();

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
    fn assistant_reply_is_visible_in_the_panel() {
        let (mut app, _rx) = disconnected();
        app.apply_event(Event::AssistantReply(AssistantReply {
            reply: "Added bolts.".to_string(),
            changed: false,
            pending_delete: None,
        }));
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_eframe(|_cc| app);

        harness.run();
        harness.get_by_label("Assistant: Added bolts.");
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
}
