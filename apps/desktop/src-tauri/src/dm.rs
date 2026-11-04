//! Per-server direct conversations. Membership and authorization stay on the server.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::{State as ClientState, v1};
use ss_core::Id;
use tauri::State;
use ts_rs::TS;

fn id(value: &str) -> Result<bytes::Bytes, ApiError> {
    value
        .parse::<Id>()
        .map(|id| id.as_bytes().to_vec().into())
        .map_err(|_| {
            ApiError::new(
                "invalid_id",
                "Invalid participant or conversation identifier.",
            )
        })
}
fn display(value: &[u8]) -> Result<String, ApiError> {
    Id::try_from(value)
        .map(|id| id.to_string())
        .map_err(|_| ApiError::new("invalid_id", "Invalid conversation identifier from server."))
}
/// Sidebar metadata for a conversation the signed-in user belongs to.
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct DmView {
    pub id: String,
    pub name: String,
    pub kind: i32,
    pub member_ids: Vec<String>,
    pub creator_id: Option<String>,
    #[ts(type = "number")]
    pub last_activity: i64,
    pub unread: bool,
    pub mentions: u32,
}
/// Project membership-scoped conversations without exposing message bodies.
pub fn views(state: &ClientState) -> Result<Vec<DmView>, ApiError> {
    let Some(own) = state.own_user.as_ref() else {
        return Ok(Vec::new());
    };
    state
        .direct_messages
        .iter()
        .filter(|(_, channel)| channel.member_ids.contains(&own.id))
        .map(|(key, channel)| {
            let cached = state
                .messages
                .get(key)
                .and_then(|m| m.ordered().last().map(|m| m.id.clone()));
            let newest = cached
                .as_ref()
                .into_iter()
                .chain(channel.last_message_id.as_ref())
                .max();
            let read = state.read_states.get(key);
            Ok(DmView {
                id: key.to_string(),
                name: channel.name.clone(),
                kind: channel.kind,
                member_ids: channel
                    .member_ids
                    .iter()
                    .map(|raw| display(raw))
                    .collect::<Result<_, _>>()?,
                creator_id: channel.created_by.as_deref().map(display).transpose()?,
                last_activity: newest
                    .map(|raw| {
                        Id::try_from(raw.as_ref())
                            .map(|id| i64::try_from(id.timestamp_ms()).unwrap_or(i64::MAX))
                    })
                    .transpose()
                    .map_err(|_| ApiError::new("invalid_id", "Invalid last-message identifier."))?
                    .unwrap_or(channel.created_at),
                unread: newest
                    .is_some_and(|last| read.is_none_or(|r| r.last_read_message_id < *last)),
                mentions: read.map_or(0, |r| r.mention_count),
            })
        })
        .collect()
}
/// Typed DM mutations forwarded to the authenticated client.
#[derive(Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum DmAction {
    Create {
        user_ids: Vec<String>,
        name: String,
    },
    Add {
        channel: String,
        user_ids: Vec<String>,
    },
    Leave {
        channel: String,
    },
    Rename {
        channel: String,
        name: String,
    },
}
/// A successful mutation, with a recoverable naming warning when applicable.
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct DmResult {
    pub channel_id: String,
    pub warning: Option<String>,
}
/// Apply a server-authorized direct-conversation action.
#[tauri::command]
pub async fn dm_action(
    state: State<'_, AppState>,
    server_id: String,
    action: DmAction,
) -> Result<DmResult, ApiError> {
    perform(&state, &server_id, action).await
}
async fn perform(state: &AppState, server: &str, action: DmAction) -> Result<DmResult, ApiError> {
    let server = parse_server_id(server)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "That server is not connected."))?;
    let mut warning = None;
    let channel = match action {
        DmAction::Create { user_ids, name } => {
            let name = valid_name(&name)?;
            if !(1..=9).contains(&user_ids.len()) {
                return Err(ApiError::new(
                    "invalid_members",
                    "Choose between one and nine other people.",
                ));
            }
            let users = user_ids.iter().map(|u| id(u)).collect::<Result<_, _>>()?;
            let channel = client
                .create_dm(v1::CreateDm { user_ids: users })
                .await
                .map_err(api_error)?;
            let key = display(&channel.id)?;
            // Creating and naming are separate protocol commands. Never report a
            // naming failure as failed creation: retrying would duplicate a group.
            if channel.kind == v1::ChannelKind::GroupDm as i32
                && !name.is_empty()
                && let Err(error) = client
                    .update_channel(v1::UpdateChannel {
                        channel_id: channel.id,
                        name: Some(name),
                        ..Default::default()
                    })
                    .await
            {
                warning = Some(format!(
                    "Conversation created, but its name could not be saved: {}. Use Rename to retry.",
                    api_error(error).message
                ));
            }
            key
        }
        DmAction::Add { channel, user_ids } => {
            client
                .add_dm_members(v1::AddDmMembers {
                    channel_id: id(&channel)?,
                    user_ids: user_ids.iter().map(|u| id(u)).collect::<Result<_, _>>()?,
                })
                .await
                .map_err(api_error)?;
            channel
        }
        DmAction::Leave { channel } => {
            client
                .leave_dm(v1::LeaveDm {
                    channel_id: id(&channel)?,
                })
                .await
                .map_err(api_error)?;
            channel
        }
        DmAction::Rename { channel, name } => {
            let name = valid_name(&name)?;
            client
                .update_channel(v1::UpdateChannel {
                    channel_id: id(&channel)?,
                    name: Some(name),
                    ..Default::default()
                })
                .await
                .map_err(api_error)?;
            channel
        }
    };
    Ok(DmResult {
        channel_id: channel,
        warning,
    })
}
fn valid_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::new(
            "invalid_name",
            "Use a group name of at most 64 characters without control characters.",
        ));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_core::Permissions;
    #[test]
    fn projection_hides_nonmembers_and_uses_latest_cursor_not_old_history() {
        let own = Id::generate();
        let channel = Id::generate();
        let older = Id::from_bytes(1_u128.to_be_bytes());
        let newer = Id::from_bytes(2_u128.to_be_bytes());
        let bytes = |id: Id| bytes::Bytes::copy_from_slice(&id.as_bytes());
        let mut state = ClientState {
            own_user: Some(v1::User {
                id: bytes(own),
                ..Default::default()
            }),
            ..Default::default()
        };
        state.direct_messages.insert(
            channel,
            v1::Channel {
                id: bytes(channel),
                kind: 4,
                member_ids: vec![bytes(own)],
                last_message_id: Some(bytes(newer)),
                effective_permissions: Some(Permissions::VIEW_CHANNEL.bits()),
                ..Default::default()
            },
        );
        state.read_states.insert(
            channel,
            v1::ReadState {
                channel_id: bytes(channel),
                last_read_message_id: bytes(older),
                mention_count: 3,
            },
        );
        let view = views(&state).unwrap();
        assert!(view[0].unread);
        assert_eq!(view[0].mentions, 3);
        state
            .read_states
            .get_mut(&channel)
            .unwrap()
            .last_read_message_id = bytes(newer);
        assert!(!views(&state).unwrap()[0].unread);
        state
            .direct_messages
            .get_mut(&channel)
            .unwrap()
            .member_ids
            .clear();
        assert!(views(&state).unwrap().is_empty());
        assert!(valid_name(&"a".repeat(65)).is_err());
        assert!(valid_name("new\nname").is_err());
    }

    #[tokio::test]
    async fn real_server_reuses_pairs_and_enforces_group_membership() {
        use ss_client_core::{Client, Credentials, ServerAddr};
        use ss_net::{pins::PinStore, server::Endpoint, tls};
        use std::{sync::Arc, time::Duration};
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            ss_server::db::Pool::open(dir.path().join("server.db"))
                .await
                .unwrap(),
        );
        let owner_invite = ss_server::auth::ensure_owner_invite(&pool)
            .await
            .unwrap()
            .unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let port = endpoint.local_addr().unwrap().port();
        let address = || ServerAddr::new("127.0.0.1", port).unwrap();
        let key = address().id().unwrap();
        let server = Arc::new(ss_server::connection::Server::open(pool).await.unwrap());
        let task = tokio::spawn(server.run(endpoint));
        let owner = AppState::open(&dir.path().join("owner")).unwrap();
        owner.pins.set(&key, fingerprint).unwrap();
        let client = Client::connect_with_storage(
            address(),
            Credentials::Register(v1::Register {
                invite_code: owner_invite,
                username: "owner".into(),
                password: "test-password".into(),
            }),
            owner.pins.clone(),
            owner.storage.clone(),
        )
        .await
        .unwrap();
        let invite = client
            .create_invite(v1::CreateInvite::default())
            .await
            .unwrap();
        owner.clients.lock().await.insert(client).unwrap();
        let mut peers = Vec::new();
        let mut users = Vec::new();
        for username in ["alice", "bob", "carol"] {
            let state = AppState::open(&dir.path().join(username)).unwrap();
            state.pins.set(&key, fingerprint).unwrap();
            let client = Client::connect_with_storage(
                address(),
                Credentials::Register(v1::Register {
                    invite_code: invite.code.clone(),
                    username: username.into(),
                    password: "test-password".into(),
                }),
                state.pins.clone(),
                state.storage.clone(),
            )
            .await
            .unwrap();
            users.push(display(&client.state().read().own_user.as_ref().unwrap().id).unwrap());
            state.clients.lock().await.insert(client).unwrap();
            peers.push(state);
        }
        let pair = || DmAction::Create {
            user_ids: vec![users[0].clone()],
            name: String::new(),
        };
        let first = perform(&owner, key.as_str(), pair()).await.unwrap();
        let second = perform(&owner, key.as_str(), pair()).await.unwrap();
        assert_eq!(first.channel_id, second.channel_id);
        let group = perform(
            &owner,
            key.as_str(),
            DmAction::Create {
                user_ids: users[..2].to_vec(),
                name: "Game night".into(),
            },
        )
        .await
        .unwrap();
        assert!(group.warning.is_none());
        let group_id: Id = group.channel_id.parse().unwrap();
        async fn wait_for(
            state: &AppState,
            key: &ss_net::pins::ServerId,
            predicate: impl Fn(&ClientState) -> bool,
        ) {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if predicate(&state.clients.lock().await.get(key).unwrap().state().read()) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        wait_for(&owner, &key, |s| {
            s.direct_messages
                .get(&group_id)
                .is_some_and(|c| c.name == "Game night")
        })
        .await;
        let own_id = display(
            &owner
                .clients
                .lock()
                .await
                .get(&key)
                .unwrap()
                .state()
                .read()
                .own_user
                .as_ref()
                .unwrap()
                .id,
        )
        .unwrap();
        let sent = peers[0]
            .clients
            .lock()
            .await
            .get(&key)
            .unwrap()
            .send_message(v1::SendMessage {
                channel_id: id(&first.channel_id).unwrap(),
                content: format!("Hello <@{own_id}>"),
                ..Default::default()
            })
            .await
            .unwrap();
        wait_for(&owner, &key, |s| {
            views(s)
                .unwrap()
                .iter()
                .any(|dm| dm.id == first.channel_id && dm.unread && dm.mentions == 1)
        })
        .await;
        owner
            .clients
            .lock()
            .await
            .get(&key)
            .unwrap()
            .mark_read(v1::MarkRead {
                channel_id: id(&first.channel_id).unwrap(),
                message_id: sent.as_bytes().to_vec().into(),
            })
            .await
            .unwrap();
        wait_for(&owner, &key, |s| {
            views(s)
                .unwrap()
                .iter()
                .any(|dm| dm.id == first.channel_id && !dm.unread && dm.mentions == 0)
        })
        .await;
        let add = || DmAction::Add {
            channel: group.channel_id.clone(),
            user_ids: vec![users[2].clone()],
        };
        assert!(perform(&peers[0], key.as_str(), add()).await.is_err());
        perform(&owner, key.as_str(), add()).await.unwrap();
        wait_for(&peers[2], &key, |s| {
            s.direct_messages
                .get(&group_id)
                .is_some_and(|c| c.member_ids.len() == 4)
        })
        .await;
        perform(
            &peers[0],
            key.as_str(),
            DmAction::Rename {
                channel: group.channel_id.clone(),
                name: "Raid group".into(),
            },
        )
        .await
        .unwrap();
        wait_for(&owner, &key, |s| {
            s.direct_messages
                .get(&group_id)
                .is_some_and(|c| c.name == "Raid group")
        })
        .await;
        perform(
            &peers[0],
            key.as_str(),
            DmAction::Leave {
                channel: group.channel_id,
            },
        )
        .await
        .unwrap();
        wait_for(&peers[0], &key, |s| {
            !s.direct_messages.contains_key(&group_id)
        })
        .await;
        wait_for(&owner, &key, |s| {
            s.direct_messages
                .get(&group_id)
                .is_some_and(|c| c.member_ids.len() == 3)
        })
        .await;
        assert!(
            perform(
                &owner,
                key.as_str(),
                DmAction::Leave {
                    channel: first.channel_id
                }
            )
            .await
            .is_err()
        );
        let unnamed = perform(
            &owner,
            key.as_str(),
            DmAction::Create {
                user_ids: users[..2].to_vec(),
                name: "Raid\u{200b}".into(),
            },
        )
        .await
        .unwrap();
        assert!(unnamed.warning.is_some());
        let unnamed_id: Id = unnamed.channel_id.parse().unwrap();
        perform(
            &owner,
            key.as_str(),
            DmAction::Rename {
                channel: unnamed.channel_id,
                name: "Recovered name".into(),
            },
        )
        .await
        .unwrap();
        wait_for(&owner, &key, |s| {
            s.direct_messages
                .get(&unnamed_id)
                .is_some_and(|c| c.name == "Recovered name")
        })
        .await;
        task.abort();
    }
}
