//! Presentation-only projection of the client's authoritative message cache.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::{ApiError, BridgeMessage},
};
use serde::{Deserialize, Serialize};
use ss_client_core::v1;
use ss_core::{Id, Permissions};
use tauri::State;
use ts_rs::TS;

fn id(value: &str) -> Result<Id, ApiError> {
    value
        .parse()
        .map_err(|_| ApiError::new("invalid_id", "Invalid message or channel identifier."))
}
fn display(bytes: &[u8]) -> Result<String, ApiError> {
    Id::try_from(bytes)
        .map(|id| id.to_string())
        .map_err(|_| ApiError::new("invalid_id", "Invalid server identifier."))
}
pub(crate) fn message_view(message: &v1::Message) -> Result<BridgeMessage, ApiError> {
    Ok(BridgeMessage {
        id: display(&message.id)?,
        channel_id: display(&message.channel_id)?,
        author_name: message
            .author
            .as_ref()
            .map(|a| a.display_name.clone())
            .or_else(|| message.webhook_name.clone()),
        author_id: message
            .author
            .as_ref()
            .map(|a| display(&a.id))
            .transpose()?,
        username: message.author.as_ref().map(|a| a.username.clone()),
        reply_to: message.reply_to.as_deref().map(display).transpose()?,
        content: message.content.clone(),
        created_at: message.created_at,
        edited_at: message.edited_at,
        attachment_count: message.attachments.len().try_into().unwrap_or(u32::MAX),
    })
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ChatSnapshot {
    messages: Vec<BridgeMessage>,
    own_user_id: Option<String>,
    last_read: Option<String>,
    can_send: bool,
    can_manage: bool,
}
#[tauri::command]
pub async fn chat_snapshot(
    state: State<'_, AppState>,
    server_id: String,
    channel_id: String,
) -> Result<ChatSnapshot, ApiError> {
    snapshot_for(&state, &server_id, &channel_id).await
}
async fn snapshot_for(
    state: &AppState,
    server_id: &str,
    channel_id: &str,
) -> Result<ChatSnapshot, ApiError> {
    let server = parse_server_id(server_id)?;
    let channel = id(channel_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "That server is not connected."))?;
    let store = client.state();
    store.retain_loaded_messages(channel);
    let view = store.read();
    let permissions = view.permissions(channel).unwrap_or_else(Permissions::empty);
    if !permissions.contains(Permissions::VIEW_CHANNEL) {
        return Err(ApiError::new(
            "forbidden",
            "This channel is no longer visible.",
        ));
    }
    Ok(ChatSnapshot {
        messages: view
            .messages
            .get(&channel)
            .map(|cache| cache.ordered().into_iter().map(message_view).collect())
            .transpose()?
            .unwrap_or_default(),
        own_user_id: view.own_user.as_ref().map(|u| display(&u.id)).transpose()?,
        last_read: view
            .read_states
            .get(&channel)
            .filter(|r| !r.last_read_message_id.is_empty())
            .map(|r| display(&r.last_read_message_id))
            .transpose()?,
        can_send: permissions.contains(Permissions::SEND_MESSAGES),
        can_manage: permissions.contains(Permissions::MANAGE_MESSAGES),
    })
}
#[derive(Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum ChatAction {
    Edit { message: String, content: String },
    Delete { message: String },
    Read { channel: String, message: String },
}
#[tauri::command]
pub async fn chat_action(
    state: State<'_, AppState>,
    server_id: String,
    action: ChatAction,
) -> Result<(), ApiError> {
    action_for(&state, &server_id, action).await
}
async fn action_for(state: &AppState, server_id: &str, action: ChatAction) -> Result<(), ApiError> {
    let server = parse_server_id(server_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "That server is not connected."))?;
    match action {
        ChatAction::Edit { message, content } => {
            client
                .edit_message(v1::EditMessage {
                    message_id: id(&message)?.as_bytes().to_vec().into(),
                    content,
                })
                .await
        }
        ChatAction::Delete { message } => {
            client
                .delete_message(v1::DeleteMessage {
                    message_id: id(&message)?.as_bytes().to_vec().into(),
                })
                .await
        }
        ChatAction::Read { channel, message } => {
            client
                .mark_read(v1::MarkRead {
                    channel_id: id(&channel)?.as_bytes().to_vec().into(),
                    message_id: id(&message)?.as_bytes().to_vec().into(),
                })
                .await
        }
    }
    .map_err(api_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn live_bridge_tracks_send_edit_read_and_delete() {
        use ss_client_core::{Client, Credentials, ServerAddr};
        use ss_net::{pins::PinStore, server::Endpoint, tls};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            ss_server::db::Pool::open(dir.path().join("server.db"))
                .await
                .unwrap(),
        );
        let invite = ss_server::auth::ensure_owner_invite(&pool)
            .await
            .unwrap()
            .unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let server = Arc::new(ss_server::connection::Server::open(pool).await.unwrap());
        let task = tokio::spawn(server.run(endpoint.clone()));
        let state = AppState::open(&dir.path().join("desktop")).unwrap();
        let address = ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        let key = address.id().unwrap();
        state.pins.set(&key, fingerprint).unwrap();
        let client = Client::connect_with_storage(
            address,
            Credentials::Register(v1::Register {
                invite_code: invite,
                username: "owner".into(),
                password: "test-password".into(),
            }),
            state.pins.clone(),
            state.storage.clone(),
        )
        .await
        .unwrap();
        let channel = *client
            .state()
            .read()
            .channels
            .iter()
            .find(|(_, c)| c.kind == v1::ChannelKind::Text as i32)
            .unwrap()
            .0;
        let message = client
            .send_message(v1::SendMessage {
                channel_id: channel.as_bytes().to_vec().into(),
                content: "hello".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        state.clients.lock().await.insert(client).unwrap();
        async fn wait_for(
            state: &AppState,
            server: &str,
            channel: &str,
            predicate: impl Fn(&ChatSnapshot) -> bool,
        ) {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if predicate(&snapshot_for(state, server, channel).await.unwrap()) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        let channel = channel.to_string();
        wait_for(&state, key.as_str(), &channel, |v| {
            v.messages.len() == 1
                && v.can_send
                && v.messages[0].username.as_deref() == Some("owner")
        })
        .await;
        action_for(
            &state,
            key.as_str(),
            ChatAction::Edit {
                message: message.to_string(),
                content: "updated".into(),
            },
        )
        .await
        .unwrap();
        wait_for(&state, key.as_str(), &channel, |v| {
            v.messages[0].content == "updated" && v.messages[0].edited_at.is_some()
        })
        .await;
        action_for(
            &state,
            key.as_str(),
            ChatAction::Read {
                channel: channel.clone(),
                message: message.to_string(),
            },
        )
        .await
        .unwrap();
        wait_for(&state, key.as_str(), &channel, |v| {
            v.last_read.as_deref() == Some(&message.to_string())
        })
        .await;
        action_for(
            &state,
            key.as_str(),
            ChatAction::Delete {
                message: message.to_string(),
            },
        )
        .await
        .unwrap();
        wait_for(&state, key.as_str(), &channel, |v| v.messages.is_empty()).await;
        assert!(
            snapshot_for(&state, key.as_str(), &Id::generate().to_string())
                .await
                .is_err()
        );
        assert!(
            action_for(
                &state,
                key.as_str(),
                ChatAction::Delete {
                    message: "bad id".into()
                }
            )
            .await
            .is_err()
        );
        task.abort();
    }
    #[test]
    fn projection_preserves_identity_reply_and_untrusted_text() {
        let message = v1::Message {
            id: Id::generate().as_bytes().to_vec().into(),
            channel_id: Id::generate().as_bytes().to_vec().into(),
            author: Some(v1::User {
                id: Id::generate().as_bytes().to_vec().into(),
                username: "immutable".into(),
                display_name: "Someone else".into(),
                ..Default::default()
            }),
            content: "<script>alert(1)</script>".into(),
            reply_to: Some(Id::generate().as_bytes().to_vec().into()),
            ..Default::default()
        };
        let view = message_view(&message).unwrap();
        assert_eq!(view.username.as_deref(), Some("immutable"));
        assert_eq!(view.content, message.content);
        assert_eq!(
            view.reply_to,
            Some(display(message.reply_to.as_ref().unwrap()).unwrap())
        );
        assert!(view.author_id.is_some());
        let mut webhook = message;
        webhook.author = None;
        webhook.webhook_name = Some("Integration".into());
        let view = message_view(&webhook).unwrap();
        assert!(view.username.is_none());
        assert_eq!(view.author_name.as_deref(), Some("Integration"));
    }
}
