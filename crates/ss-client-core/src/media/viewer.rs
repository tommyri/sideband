//! One uni-stream demultiplexer per connection, with one pending GOP per viewer.
use super::GroupReader;
use crate::{
    Client, ConnectionState, Error, StateStore,
    connection::{Command, MediaLink},
};
use parking_lot::Mutex;
use ss_proto::v1::{self, client_envelope::Msg};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, mpsc, oneshot};

#[derive(Default)]
struct Slot {
    group: Mutex<Option<GroupReader>>,
    notify: Notify,
    newest: Mutex<Option<u32>>,
}
/// Shared by the clones of one media link; never creates competing accept loops.
#[derive(Default)]
pub(crate) struct Hub {
    generation: u64,
    slots: Arc<Mutex<BTreeMap<u32, Arc<Slot>>>>,
}
/// Audio render timestamp paired with the local monotonic clock (not DAC latency).
#[derive(Clone, Copy, Debug)]
pub struct AudioClock {
    /// Wrapping 48 kHz position.
    pub timestamp: u32,
    /// Instant at which that position was mixed.
    pub at: Instant,
}
impl AudioClock {
    /// Extrapolate briefly; stale audio is not a synchronization reference.
    #[must_use]
    pub fn now(self) -> Option<u32> {
        let elapsed = self.at.elapsed();
        (elapsed < Duration::from_millis(250)).then(|| {
            self.timestamp
                .wrapping_add(u32::try_from(elapsed.as_micros() * 48 / 1000).unwrap_or(0))
        })
    }
}
/// A generation-bound video subscription. Drop unsubscribes and frees queued media.
pub struct VideoSubscription {
    /// Track metadata, fixed for this subscription's lifetime.
    pub track: v1::Track,
    slot: Arc<Slot>,
    slots: Arc<Mutex<BTreeMap<u32, Arc<Slot>>>>,
    commands: mpsc::Sender<Command>,
    store: StateStore,
    generation: u64,
    media: MediaLink,
}
impl VideoSubscription {
    /// Recheck on every display tick; a stale frame must not survive access loss.
    #[must_use]
    pub fn valid(&self) -> bool {
        let state = self.store.read();
        state.generation == self.generation
            && state.connection == ConnectionState::Connected
            && state.tracks.contains_key(&self.track.track_id)
            && state.voice_states.values().any(|voice| {
                state
                    .own_user
                    .as_ref()
                    .is_some_and(|user| user.id == voice.user_id)
                    && voice.channel_id.as_ref() == Some(&self.track.channel_id)
            })
            && ss_core::Id::try_from(self.track.channel_id.as_ref())
                .ok()
                .and_then(|id| state.permissions(id))
                .is_some_and(|p| {
                    p.contains(ss_core::Permissions::VIEW_CHANNEL | ss_core::Permissions::CONNECT)
                })
    }
    /// Wait for the newest GOP; superseded pending groups are reset on drop.
    pub async fn next_group(&self) -> Option<GroupReader> {
        loop {
            let notified = self.slot.notify.notified();
            if !self.valid() {
                return None;
            }
            if let Some(group) = self.slot.group.lock().take() {
                return Some(group);
            }
            tokio::select! { ()=notified=>{}, ()=tokio::time::sleep(Duration::from_millis(100))=>{} }
        }
    }
    /// Request an IDR; caller throttles recovery requests.
    pub async fn request_keyframe(&self) -> Result<(), Error> {
        self.command(Msg::RequestKeyframe(v1::RequestKeyframe {
            track_id: self.track.track_id,
        }))
        .await
    }
    /// Complete the server-authorized subscription after releasing any host client lock.
    pub async fn subscribe(&self) -> Result<(), Error> {
        self.command(Msg::SubscribeTrack(v1::SubscribeTrack {
            track_id: self.track.track_id,
        }))
        .await
    }
    async fn command(&self, message: Msg) -> Result<(), Error> {
        if !self.valid() {
            return Err(Error::Disconnected);
        }
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command {
                message,
                generation: self.generation,
                replayable: false,
                reply,
            })
            .map_err(|_| Error::Disconnected)?;
        response.await.map_err(|_| Error::Disconnected)??;
        Ok(())
    }
    /// Latest mixed timestamp for the sharer's voice, when it is playing locally.
    #[must_use]
    pub fn audio_clock(&self) -> Option<AudioClock> {
        let state = self.store.read();
        let voice = state.tracks.values().find(|t| {
            t.kind == v1::TrackKind::Audio as i32
                && t.user_id == self.track.user_id
                && t.conn_id == self.track.conn_id
        })?;
        self.media.audio_clocks.lock().get(&voice.track_id).copied()
    }
}
impl Drop for VideoSubscription {
    fn drop(&mut self) {
        self.slots.lock().remove(&self.track.track_id);
        let (reply, _) = oneshot::channel();
        let _ = self.commands.try_send(Command {
            message: Msg::UnsubscribeTrack(v1::UnsubscribeTrack {
                track_id: self.track.track_id,
            }),
            generation: self.generation,
            replayable: false,
            reply,
        });
    }
}
impl Client {
    /// Subscribe through the shared connection demultiplexer. One window per track.
    pub fn prepare_video(&self, track_id: u32) -> Result<VideoSubscription, Error> {
        let (track, generation) = {
            let state = self.store.read();
            let track = state
                .tracks
                .get(&track_id)
                .filter(|t| t.kind == v1::TrackKind::Screen as i32)
                .cloned()
                .ok_or(Error::Protocol("screen track unavailable"))?;
            (track, state.generation)
        };
        let connection = self.media.quic().ok_or(Error::Disconnected)?;
        let mut hub = self.media.video.lock();
        if hub.generation != generation {
            hub.generation = generation;
            hub.slots = Arc::default();
            let slots = hub.slots.clone();
            let store = self.store.clone();
            tokio::spawn(async move {
                loop {
                    if store.read().generation != generation
                        || store.read().connection != ConnectionState::Connected
                    {
                        break;
                    }
                    tokio::select! {
                        incoming=connection.accept_uni()=>{
                            let Ok(incoming)=incoming else {break;};
                            if let Ok(Ok(group))=tokio::time::timeout(Duration::from_secs(1),GroupReader::from_stream(incoming)).await
                                && let Some(slot)=slots.lock().get(&group.header.media.track_id) {
                                    let mut newest=slot.newest.lock();
                                    if newest.is_none_or(|old|group.header.group_id.wrapping_sub(old).cast_signed()>0) {
                                        *newest=Some(group.header.group_id); *slot.group.lock()=Some(group); slot.notify.notify_one();
                                    }
                            }
                        },
                        ()=tokio::time::sleep(Duration::from_millis(100))=>{}
                    }
                }
                slots.lock().clear();
            });
        }
        let slots = hub.slots.clone();
        drop(hub);
        let slot = Arc::new(Slot::default());
        {
            let mut slots = slots.lock();
            if slots.contains_key(&track_id) || slots.len() >= 8 {
                return Err(Error::Protocol("viewer already open or limit reached"));
            }
            slots.insert(track_id, slot.clone());
        }
        let subscription = VideoSubscription {
            track,
            slot,
            slots,
            commands: self.commands.clone(),
            store: self.store.clone(),
            generation,
            media: self.media.clone(),
        };
        Ok(subscription)
    }
}
