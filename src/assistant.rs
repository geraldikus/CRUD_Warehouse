//! `POST /assistant`. Groq may call warehouse functions; it never writes SQL.
//!
//! One HTTP request can contain several model turns. The visible chat is
//! only user and assistant text. Tool calls and their results live in
//! `messages` for this request and are discarded when the handler returns.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppError;
use crate::models::{
    AssistantReply, AssistantRequest, ChatMessage, ChatRole, CreateItem, PendingAdd,
    PendingAddMatch, PendingDelete, UpdateItem,
};
use crate::repo;

/// How many model turns one user message may trigger. Each turn is one
/// paid HTTP call. The cap also stops a model that keeps calling tools.
const MAX_STEPS: usize = 5;

pub async fn handle(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(request): axum::Json<AssistantRequest>,
) -> Result<axum::Json<AssistantReply>, AppError> {
    let reply = run(&state, request).await?;
    Ok(axum::Json(reply))
}

async fn run(state: &AppState, request: AssistantRequest) -> Result<AssistantReply, AppError> {
    let Some(llm) = state.llm.as_ref() else {
        return Err(AppError::AssistantUnavailable);
    };

    let mut messages = chat_to_messages(&request.messages)?;
    // Built once so "end of the month" means the same thing on every step.
    let system = system_instruction(Utc::now());
    messages.insert(0, json!({ "role": "system", "content": system }));
    let tools = tools();
    let mut changed = false;
    let mut pending_delete = None;
    let mut pending_add = None;

    for _step in 0..MAX_STEPS {
        let response = llm
            .generate(&messages, &tools)
            .await
            .map_err(AppError::Llm)?;
        let message = assistant_message(&response).map_err(AppError::Llm)?;
        let calls = tool_calls(&message).map_err(AppError::Llm)?;
        if calls.is_empty() {
            let text = text_from(&message).trim().to_string();
            return finalize_reply(
                state,
                &request,
                AssistantReply {
                    reply: if text.is_empty() {
                        "Done.".to_string()
                    } else {
                        text
                    },
                    changed,
                    pending_delete,
                    pending_add,
                },
            )
            .await;
        }

        let mut effects = Vec::with_capacity(calls.len());
        for call in &calls {
            tracing::info!(function = %call.name, "assistant tool call");
            let effect = if call.name == "delete_item" && pending_delete.is_some() {
                ToolEffect::message("only one deletion can wait for confirmation at a time")
            } else if call.name == "create_item" && pending_add.is_some() {
                ToolEffect::message("only one add can wait for confirmation at a time")
            } else {
                execute(state, call).await?
            };
            if effect.changed {
                changed = true;
            }
            if effect.pending_delete.is_some() {
                pending_delete = effect.pending_delete.clone();
            }
            if effect.pending_add.is_some() {
                pending_add = effect.pending_add.clone();
            }
            effects.push(effect);
        }

        // Groq requires the assistant turn, including every `tool_calls` id,
        // before the matching `role: tool` results.
        messages.push(assistant_tool_message(&message));
        for (call, effect) in calls.iter().zip(effects) {
            messages.push(tool_result_message(&call.id, &effect.response));
        }
    }

    finalize_reply(
        state,
        &request,
        AssistantReply {
            reply: "I stopped after several steps without a final answer.".to_string(),
            changed,
            pending_delete,
            pending_add,
        },
    )
    .await
}

fn system_instruction(now: DateTime<Utc>) -> String {
    format!(
        "You are the warehouse assistant for an inventory app. \
         Manage stock only through the provided functions. \
         Do not invent item ids: call list_items first when the user names an item. \
         quantity must be zero or greater. \
         expires_at is an RFC 3339 UTC timestamp such as 2026-11-19T23:59:59Z, \
         or JSON null when the item does not expire. \
         When the user gives a calendar date without a time, convert it to the end of that day in UTC. \
         The current time is {now}. \
         Reply in the same language the user uses. \
         Never claim a change succeeded unless the function result says so. \
         delete_item does not remove the row. Tell the user to press Confirm delete. \
         If create_item finds a row with the same name, nothing is written. \
         Tell the user to press Add to existing or Create new. \
         When the user asks to add stock, always call create_item with the amount to add. \
         Do not use update_item to add more quantity."
    )
}

/// If the model skipped `create_item` but the user clearly asked to add stock
/// and the name already exists, attach `pending_add` so the UI can show buttons.
async fn finalize_reply(
    state: &AppState,
    request: &AssistantRequest,
    mut reply: AssistantReply,
) -> Result<AssistantReply, AppError> {
    if reply.changed || reply.pending_delete.is_some() || reply.pending_add.is_some() {
        return Ok(reply);
    }

    let Some(user) = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::User)
    else {
        return Ok(reply);
    };
    let Some((quantity, name)) = parse_add_intent(&user.text) else {
        return Ok(reply);
    };
    if crate::validation::validate_item(&name, quantity).is_err() {
        return Ok(reply);
    }

    let matches = repo::find_items_by_name(&state.pool, &name).await?;
    if matches.is_empty() {
        return Ok(reply);
    }

    reply.pending_add = Some(PendingAdd {
        name,
        quantity,
        expires_at: None,
        matches: matches.iter().map(pending_add_match).collect(),
    });
    Ok(reply)
}

/// `add 10 cow` or `Add 10 cow`.
fn parse_add_intent(text: &str) -> Option<(i32, String)> {
    let text = text.trim();
    if text.len() < 3 || !text.as_bytes()[..3].eq_ignore_ascii_case(b"add") {
        return None;
    }
    match text.as_bytes().get(3) {
        None => return None,
        Some(b) if b.is_ascii_whitespace() => {}
        Some(_) => return None,
    }
    let rest = text.get(3..)?.trim_start();
    let (qty_str, name) = rest.split_once(char::is_whitespace)?;
    let quantity: i32 = qty_str.parse().ok()?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some((quantity, name.to_string()))
}

fn tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_items",
                "description": "List every warehouse item, newest first. Call this before updating or deleting when you only know the name.",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_item",
                "description": "Add stock. If a row already uses this name, the app asks the user to add to that row or create a new one. quantity must be >= 0. expires_at is RFC 3339 UTC or null.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Item name" },
                        "quantity": { "type": "integer", "description": "How many, zero or more" },
                        "expires_at": {
                            "type": ["string", "null"],
                            "description": "RFC 3339 UTC timestamp, or null if it does not expire"
                        }
                    },
                    "required": ["name", "quantity"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "update_item",
                "description": "Set the name, quantity, and expiry of an existing item by id. Do not use this to add more stock when the user says add; call create_item instead. Null expires_at clears the date.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Item UUID from list_items" },
                        "name": { "type": "string" },
                        "quantity": { "type": "integer" },
                        "expires_at": {
                            "type": ["string", "null"],
                            "description": "RFC 3339 UTC timestamp, or null to clear the date"
                        }
                    },
                    "required": ["id", "name", "quantity"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delete_item",
                "description": "Ask the user to confirm deletion. This does not delete the row. The app shows a Confirm delete button.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Item UUID from list_items" }
                    },
                    "required": ["id"]
                }
            }
        }
    ])
}

struct FunctionCall {
    id: String,
    name: String,
    args: Result<Value, String>,
}

struct ToolEffect {
    response: Value,
    changed: bool,
    pending_delete: Option<PendingDelete>,
    pending_add: Option<PendingAdd>,
}

impl ToolEffect {
    fn message(text: &str) -> Self {
        Self {
            response: json!({ "error": text }),
            changed: false,
            pending_delete: None,
            pending_add: None,
        }
    }
}

#[derive(Deserialize)]
struct UpdateArgs {
    id: Uuid,
    #[serde(flatten)]
    item: UpdateItem,
}

#[derive(Deserialize)]
struct DeleteArgs {
    id: Uuid,
}

async fn execute(state: &AppState, call: &FunctionCall) -> Result<ToolEffect, AppError> {
    let args = match &call.args {
        Ok(args) => args.clone(),
        Err(err) => return Ok(ToolEffect::message(err)),
    };

    match call.name.as_str() {
        "list_items" => {
            let items = repo::list_items(&state.pool).await?;
            Ok(ToolEffect {
                response: json!({ "items": items }),
                changed: false,
                pending_delete: None,
                pending_add: None,
            })
        }
        "create_item" => match serde_json::from_value::<CreateItem>(args) {
            Ok(input) => create_or_ask(state, input).await,
            Err(err) => Ok(ToolEffect::message(&format!("invalid arguments: {err}"))),
        },
        "update_item" => match serde_json::from_value::<UpdateArgs>(args) {
            Ok(input) => {
                match repo::update_item(&state.pool, &state.mqtt, input.id, input.item).await {
                    Ok(item) => Ok(ToolEffect {
                        response: json_item(&item),
                        changed: true,
                        pending_delete: None,
                        pending_add: None,
                    }),
                    Err(AppError::Validation(message)) => Ok(ToolEffect::message(&message)),
                    Err(AppError::NotFound) => Ok(ToolEffect::message("item not found")),
                    Err(other) => Err(other),
                }
            }
            Err(err) => Ok(ToolEffect::message(&format!("invalid arguments: {err}"))),
        },
        "delete_item" => match serde_json::from_value::<DeleteArgs>(args) {
            Ok(input) => match repo::get_item(&state.pool, input.id).await? {
                Some(item) => Ok(ToolEffect {
                    response: json!({
                        "status": "confirmation_required",
                        "id": item.id,
                        "name": item.name,
                        "message": "The item is still stored. The user must press Confirm delete."
                    }),
                    changed: false,
                    pending_delete: Some(PendingDelete {
                        id: item.id,
                        name: item.name,
                    }),
                    pending_add: None,
                }),
                None => Ok(ToolEffect::message("item not found")),
            },
            Err(err) => Ok(ToolEffect::message(&format!("invalid arguments: {err}"))),
        },
        other => Ok(ToolEffect::message(&format!("unknown function: {other}"))),
    }
}

async fn create_or_ask(state: &AppState, input: CreateItem) -> Result<ToolEffect, AppError> {
    if let Err(err) = crate::validation::validate_item(input.name.trim(), input.quantity) {
        return Ok(ToolEffect::message(&err.to_string()));
    }

    let matches = repo::find_items_by_name(&state.pool, input.name.trim()).await?;
    if !matches.is_empty() {
        let pending = PendingAdd {
            name: input.name.trim().to_string(),
            quantity: input.quantity,
            expires_at: input.expires_at,
            matches: matches.iter().map(pending_add_match).collect(),
        };
        return Ok(ToolEffect {
            response: json!({
                "status": "confirmation_required",
                "name": pending.name,
                "quantity": pending.quantity,
                "message": "A row with this name already exists. The user must press Add to existing or Create new."
            }),
            changed: false,
            pending_delete: None,
            pending_add: Some(pending),
        });
    }

    match repo::create_item(&state.pool, &state.mqtt, input).await {
        Ok(item) => Ok(ToolEffect {
            response: json_item(&item),
            changed: true,
            pending_delete: None,
            pending_add: None,
        }),
        Err(AppError::Validation(message)) => Ok(ToolEffect::message(&message)),
        Err(other) => Err(other),
    }
}

fn pending_add_match(item: &crate::models::Item) -> PendingAddMatch {
    PendingAddMatch {
        id: item.id,
        name: item.name.clone(),
        quantity: item.quantity,
        expires_at: item.expires_at,
    }
}

fn json_item(item: &crate::models::Item) -> Value {
    serde_json::to_value(item).unwrap_or_else(|_| json!({ "error": "could not encode item" }))
}

fn chat_to_messages(messages: &[ChatMessage]) -> Result<Vec<Value>, AppError> {
    if messages.is_empty() {
        return Err(AppError::Validation(
            "messages must not be empty".to_string(),
        ));
    }

    let mut history = Vec::new();
    for message in messages {
        let text = message.text.trim();
        if text.is_empty() {
            return Err(AppError::Validation(
                "message text must not be empty".to_string(),
            ));
        }
        let role = match message.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        };
        push_text(&mut history, role, text);
    }

    if history
        .first()
        .and_then(|entry| entry.get("role"))
        .and_then(|role| role.as_str())
        != Some("user")
    {
        return Err(AppError::Validation(
            "the conversation must start with a user message".to_string(),
        ));
    }

    Ok(history)
}

fn push_text(messages: &mut Vec<Value>, role: &str, text: &str) {
    // Read the previous line first. A second borrow can then replace it.
    let merged = messages.last().and_then(|last| {
        if last.get("role").and_then(|value| value.as_str()) != Some(role) {
            return None;
        }
        // A tool turn has no plain `content` string to merge into.
        last.get("content")
            .and_then(|value| value.as_str())
            .map(|existing| format!("{existing}\n{text}"))
    });

    if let Some(merged) = merged {
        if let Some(slot) = messages.last_mut().and_then(|last| last.get_mut("content")) {
            *slot = Value::String(merged);
        }
        return;
    }

    messages.push(json!({
        "role": role,
        "content": text
    }));
}

fn assistant_message(response: &Value) -> Result<Value, String> {
    response
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| "groq returned no message".to_string())
}

fn tool_calls(message: &Value) -> Result<Vec<FunctionCall>, String> {
    let Some(calls) = message.get("tool_calls").and_then(|value| value.as_array()) else {
        return Ok(Vec::new());
    };

    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let id = call
                .get("id")
                .and_then(|value| value.as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{index}"));
            let function = call
                .get("function")
                .ok_or_else(|| "tool call is missing function".to_string())?;
            let name = function
                .get("name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "tool call is missing a name".to_string())?
                .to_string();
            Ok(FunctionCall {
                id,
                name,
                args: parse_arguments(function.get("arguments")),
            })
        })
        .collect()
}

/// Groq sends `arguments` as a JSON string. An object is accepted too, so a
/// test double can pass either shape.
fn parse_arguments(value: Option<&Value>) -> Result<Value, String> {
    match value {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(json!({})),
        Some(Value::String(text)) => {
            serde_json::from_str(text).map_err(|err| format!("tool arguments were not json: {err}"))
        }
        Some(Value::Object(_)) => Ok(value.cloned().unwrap_or_else(|| json!({}))),
        Some(other) => Err(format!("unexpected tool arguments: {other}")),
    }
}

fn text_from(message: &Value) -> String {
    message
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

fn assistant_tool_message(message: &Value) -> Value {
    json!({
        "role": "assistant",
        "content": message.get("content").cloned().unwrap_or(Value::Null),
        "tool_calls": message.get("tool_calls").cloned().unwrap_or_else(|| json!([]))
    })
}

fn tool_result_message(id: &str, response: &Value) -> Value {
    let content = serde_json::to_string(response)
        .unwrap_or_else(|_| r#"{"error":"could not encode tool result"}"#.to_string());
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": content
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ChatRole;

    #[test]
    fn tool_call_parses_a_json_string_of_arguments() {
        let message = json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "create_item",
                    "arguments": "{\"name\":\"bolts\",\"quantity\":3}"
                }
            }]
        });
        let calls = tool_calls(&message).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "create_item");
        assert_eq!(calls[0].args.as_ref().unwrap()["quantity"], 3);
    }

    #[test]
    fn missing_arguments_are_an_empty_object() {
        let message = json!({
            "tool_calls": [{
                "function": { "name": "list_items" }
            }]
        });
        let calls = tool_calls(&message).unwrap();
        assert_eq!(calls[0].id, "call_0");
        assert_eq!(calls[0].args.as_ref().unwrap(), &json!({}));
    }

    #[test]
    fn broken_argument_string_is_a_call_error() {
        let message = json!({
            "tool_calls": [{
                "id": "call_1",
                "function": { "name": "create_item", "arguments": "not-json" }
            }]
        });
        let calls = tool_calls(&message).unwrap();
        assert!(calls[0].args.as_ref().unwrap_err().contains("not json"));
    }

    #[test]
    fn messages_keep_assistant_role_and_merge_the_same_role() {
        let messages = vec![
            ChatMessage {
                role: ChatRole::User,
                text: " hi ".to_string(),
            },
            ChatMessage {
                role: ChatRole::User,
                text: "there".to_string(),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                text: "ok".to_string(),
            },
        ];
        let history = chat_to_messages(&messages).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[0]["content"], "hi\nthere");
        assert_eq!(history[1]["role"], "assistant");
        assert_eq!(history[1]["content"], "ok");
    }

    #[test]
    fn messages_reject_an_empty_transcript_and_an_assistant_opening() {
        match chat_to_messages(&[]).unwrap_err() {
            AppError::Validation(message) => assert!(message.contains("messages"), "{message}"),
            other => panic!("expected validation, got {other:?}"),
        }

        let opening = chat_to_messages(&[ChatMessage {
            role: ChatRole::Assistant,
            text: "hello".to_string(),
        }])
        .unwrap_err();
        match opening {
            AppError::Validation(message) => assert!(message.contains("user message")),
            other => panic!("expected validation, got {other:?}"),
        }
    }

    #[test]
    fn create_item_schema_requires_name_and_quantity() {
        let declared = tools();
        let create = declared
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "create_item")
            .unwrap();
        assert_eq!(create["type"], "function");
        assert_eq!(
            create["function"]["parameters"]["required"],
            json!(["name", "quantity"])
        );
        assert_eq!(
            create["function"]["parameters"]["properties"]["expires_at"]["type"],
            json!(["string", "null"])
        );
        let update = declared
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "update_item")
            .unwrap();
        assert_eq!(
            update["function"]["parameters"]["properties"]["expires_at"]["type"],
            json!(["string", "null"])
        );
        let names: Vec<_> = declared
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["list_items", "create_item", "update_item", "delete_item"]
        );
    }

    #[test]
    fn system_instruction_includes_the_clock_and_the_confirm_button() {
        let now = DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let text = system_instruction(now);
        assert!(text.contains("2026-11-08"), "{text}");
        assert!(text.contains("Confirm delete"), "{text}");
        assert!(text.contains("Add to existing"), "{text}");
        assert!(text.contains("create_item"), "{text}");
    }

    #[test]
    fn parse_add_intent_reads_quantity_and_name() {
        assert_eq!(
            parse_add_intent("Add 10 cow"),
            Some((10, "cow".to_string()))
        );
        assert_eq!(
            parse_add_intent("add 2 big crate"),
            Some((2, "big crate".to_string()))
        );
        assert!(parse_add_intent("update 5 bolts").is_none());
        assert!(parse_add_intent("add 5").is_none());
        assert!(parse_add_intent("address book").is_none());
    }
}
