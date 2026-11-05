//! Desktop-owned capture workers. Native frames never cross a thread or webview.
use crate::{commands::parse_server_id, state::AppState, types::ApiError};
use serde::Serialize;
use ss_client_core::{media::ScreenShareSession, v1};
use ss_core::{Id, Permissions};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{Manager, State};
use tokio::sync::{oneshot, watch};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    id: String,
    label: String,
    kind: String,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareView {
    server: String,
    channel: String,
    source: String,
    state: String,
    viewers: u32,
    error: Option<String>,
}
struct Active {
    stop: watch::Sender<bool>,
    status: Arc<Mutex<ShareView>>,
}
#[derive(Default)]
pub struct Sharing {
    sources: Mutex<BTreeMap<String, ss_video::Source>>,
    active: Mutex<Option<Active>>,
}
fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("screen_share", e.to_string())
}
fn can_publish(state: &ss_client_core::State, channel: Id, connection: Id) -> bool {
    state.connection == ss_client_core::ConnectionState::Connected
        && state
            .voice_states
            .get(&connection)
            .is_some_and(|v| v.channel_id.as_deref() == Some(channel.as_bytes().as_slice()))
        && state.permissions(channel).is_some_and(|p| {
            p.contains(Permissions::STREAM | Permissions::CONNECT | Permissions::VIEW_CHANNEL)
        })
}
// Construct and poll the !Send capture future on the same thread.
async fn native<T, F, Fut>(work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, ApiError>>,
{
    let (send, receive) = oneshot::channel();
    std::thread::Builder::new()
        .name("screen-source".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(error)
                .and_then(|runtime| runtime.block_on(work()));
            let _ = send.send(result);
        })
        .map_err(error)?;
    receive.await.map_err(error)?
}
#[tauri::command]
pub async fn screen_sources(state: State<'_, Sharing>) -> Result<Vec<SourceView>, ApiError> {
    let sources = native(|| async { ss_video::list_sources().await.map_err(error) }).await?;
    let mut registry = state.sources.lock().unwrap();
    registry.clear();
    let mut result = Vec::new();
    for source in sources {
        let kind = match source.kind {
            ss_video::SourceKind::Display => "display",
            ss_video::SourceKind::Window => "window",
            ss_video::SourceKind::Portal => "portal",
        };
        let key = format!("{kind}:{}", source.id);
        result.push(SourceView {
            id: key.clone(),
            label: source.label.clone(),
            kind: kind.into(),
        });
        registry.insert(key, source);
    }
    Ok(result)
}
#[derive(Serialize)]
pub struct Preview {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}
#[tauri::command]
pub async fn screen_preview(
    state: State<'_, Sharing>,
    source: String,
) -> Result<Preview, ApiError> {
    let source = state
        .sources
        .lock()
        .unwrap()
        .get(&source)
        .cloned()
        .ok_or_else(|| error("Refresh the source list."))?;
    if source.kind == ss_video::SourceKind::Portal {
        return Err(error(
            "The system picker previews sources when sharing starts.",
        ));
    }
    native(move || async move {
        let preview = tokio::time::timeout(Duration::from_secs(8), ss_video::thumbnail(&source))
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(Preview {
            width: preview.width,
            height: preview.height,
            rgba: preview.rgba,
        })
    })
    .await
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackView {
    track: u32,
    user: String,
    channel: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    can_share: bool,
    tracks: Vec<TrackView>,
    share: Option<ShareView>,
    watching: Vec<u32>,
}
#[tauri::command]
pub async fn screen_snapshot(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    server_id: String,
) -> Result<Snapshot, ApiError> {
    let server = parse_server_id(&server_id)?;
    let store = state
        .clients
        .lock()
        .await
        .get(&server)
        .ok_or_else(|| error("Server disconnected"))?
        .state();
    let active = state.voice.lock().await;
    let channel = active
        .as_ref()
        .filter(|v| v.server == server)
        .map(|v| (v.session.channel(), v.connection));
    drop(active);
    let snapshot = store.read();
    let can_share =
        channel.is_some_and(|(channel, connection)| can_publish(&snapshot, channel, connection));
    let tracks = snapshot
        .tracks
        .values()
        .filter(|t| t.kind == v1::TrackKind::Screen as i32)
        .filter_map(|t| {
            Some(TrackView {
                track: t.track_id,
                user: Id::try_from(t.user_id.as_ref()).ok()?.to_string(),
                channel: Id::try_from(t.channel_id.as_ref()).ok()?.to_string(),
            })
        })
        .collect();
    let share = app
        .state::<Sharing>()
        .active
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.status.lock().unwrap().clone());
    let watching = crate::viewer::stream_viewers(app.state())
        .into_iter()
        .filter(|v| v.server == server_id)
        .map(|v| v.track)
        .collect();
    Ok(Snapshot {
        can_share,
        tracks,
        share,
        watching,
    })
}
#[tauri::command]
pub async fn start_screen_share(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    channel_id: String,
    source: String,
    preset: String,
    cursor: bool,
) -> Result<(), ApiError> {
    let server = parse_server_id(&server_id)?;
    let channel: Id = channel_id.parse().map_err(error)?;
    let connection = {
        let voice = state.voice.lock().await;
        voice
            .as_ref()
            .filter(|v| v.server == server && v.session.channel() == channel)
            .map(|v| v.connection)
            .ok_or_else(|| error("Join this voice channel before sharing."))?
    };
    let (handle, store) = {
        let clients = state.clients.lock().await;
        let client = clients
            .get(&server)
            .ok_or_else(|| error("Server disconnected"))?;
        if !can_publish(&client.state().read(), channel, connection) {
            return Err(error(
                "Join this voice channel with STREAM permission before sharing.",
            ));
        }
        (client.screen_share_handle(), client.state())
    };
    let preset = match preset.as_str() {
        "1080p60" => ss_video::Preset::FullHd60,
        "1080p30" => ss_video::Preset::FullHd30,
        "720p30" => ss_video::Preset::Hd30,
        _ => return Err(error("Unknown stream preset")),
    };
    let sharing = app.state::<Sharing>();
    let source = sharing
        .sources
        .lock()
        .unwrap()
        .get(&source)
        .cloned()
        .ok_or_else(|| error("Refresh the source list."))?;
    let mut active = sharing.active.lock().unwrap();
    if active.as_ref().is_some_and(|a| !a.stop.is_closed()) {
        return Err(error("Stop the current screen share first."));
    }
    let (stop, mut stopped) = watch::channel(false);
    let status = Arc::new(Mutex::new(ShareView {
        server: server_id,
        channel: channel_id,
        source: source.label.clone(),
        state: "starting".into(),
        ..Default::default()
    }));
    let worker_status = status.clone();
    let app = app.clone();
    std::thread::Builder::new()
        .name("screen-publisher".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async {
                        let run = async {
                            let config = ss_video::PublisherConfig {
                                source,
                                preset,
                                cursor,
                                hardware_only: false,
                            };
                            let (mut session, _) =
                                ScreenShareSession::start_owned(handle.clone(), config)
                                    .await
                                    .map_err(|e| e.to_string())?;
                            worker_status.lock().unwrap().state = "sharing".into();
                            loop {
                                session.publish_next().await.map_err(|e| e.to_string())?;
                                worker_status.lock().unwrap().viewers =
                                    session.subscriber_hint().viewer_count;
                            }
                            #[allow(unreachable_code)]
                            Ok::<(), String>(())
                        };
                        // Watch membership independently of frame delivery, including
                        // while a portal or permission dialog is still open.
                        let invalidated = async {
                            loop {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                let state = app.state::<AppState>();
                                let voice = state.voice.lock().await;
                                let permitted = can_publish(&store.read(), channel, connection);
                                let joined = voice.as_ref().is_some_and(|v| {
                                    v.server == server && v.session.channel() == channel
                                });
                                if !handle.valid() || !permitted || !joined {
                                    break;
                                }
                            }
                        };
                        let result = tokio::select! {
                            result = run => result,
                            _ = stopped.changed() => Ok(()),
                            _ = invalidated => Ok(()),
                        };
                        // A canceled startup may already have queued StartStream.
                        // This ordered, generation-bound stop also covers that race.
                        let _ = tokio::time::timeout(
                            Duration::from_secs(2),
                            handle.stop_stream(v1::StopStream {}),
                        )
                        .await;
                        result
                    })
                });
            let mut status = worker_status.lock().unwrap();
            status.state = "stopped".into();
            status.viewers = 0;
            status.error = result.err();
        })
        .map_err(error)?;
    *active = Some(Active { stop, status });
    Ok(())
}
#[tauri::command]
pub fn stop_screen_share(state: State<'_, Sharing>) {
    if let Some(active) = state.active.lock().unwrap().as_ref() {
        let mut status = active.status.lock().unwrap();
        if status.state != "stopped" {
            status.state = "stopping".into();
            let _ = active.stop.send(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_requires_live_membership_and_permissions_for_the_exact_connection() {
        let channel = Id::generate();
        let connection = Id::generate();
        let mut state = ss_client_core::State {
            connection: ss_client_core::ConnectionState::Connected,
            ..Default::default()
        };
        state.channels.insert(
            channel,
            v1::Channel {
                effective_permissions: Some(
                    (Permissions::STREAM | Permissions::CONNECT | Permissions::VIEW_CHANNEL).bits(),
                ),
                ..Default::default()
            },
        );
        state.voice_states.insert(
            connection,
            v1::VoiceState {
                channel_id: Some(channel.as_bytes().to_vec().into()),
                ..Default::default()
            },
        );
        assert!(can_publish(&state, channel, connection));
        assert!(!can_publish(&state, channel, Id::generate()));
        state.voice_states.get_mut(&connection).unwrap().channel_id =
            Some(Id::generate().as_bytes().to_vec().into());
        assert!(!can_publish(&state, channel, connection));
        state.voice_states.get_mut(&connection).unwrap().channel_id =
            Some(channel.as_bytes().to_vec().into());
        state
            .channels
            .get_mut(&channel)
            .unwrap()
            .effective_permissions = Some(Permissions::CONNECT.bits());
        assert!(!can_publish(&state, channel, connection));
    }
}
