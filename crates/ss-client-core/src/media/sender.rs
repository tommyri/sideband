//! Capture/encode integration with Sideband video groups.

use crate::{
    ConnectionState, StateStore,
    connection::{Command, MediaLink},
};
use ss_proto::v1::{self, client_envelope::Msg};
use tokio::sync::{broadcast, mpsc, oneshot};

use super::{Frame, GroupWriter, VideoTrackSender};
use crate::{Client, ClientEvent};

/// Failures while publishing one screen-share track.
#[derive(Debug, thiserror::Error)]
pub enum ScreenShareError {
    /// Control-plane publication failed.
    #[error(transparent)]
    Client(#[from] crate::Error),
    /// Capture or encoding failed.
    #[error(transparent)]
    Video(#[from] ss_video::Error),
    /// QUIC group framing or transmission failed.
    #[error(transparent)]
    Stream(#[from] ss_net::media::stream::Error),
    /// The transport reconnected; the server-assigned track is no longer valid.
    #[error("screen share ended because the connection changed")]
    Reconnected,
}

/// One live screen share: capture, native encoder, and QUIC group writer.
///
/// The host repeatedly calls [`publish_next`](Self::publish_next). This is
/// pull-driven because native video frames are thread-confined; the desktop shell
/// owns capture and encoding together on a dedicated local executor.
pub struct ScreenShareSession {
    client: ScreenShareHandle,
    publisher: ss_video::Publisher,
    sender: VideoTrackSender,
    writer: Option<GroupWriter>,
    events: broadcast::Receiver<ClientEvent>,
    track: u32,
    hint: ss_video::SubscriberHint,
    clock_offset: Option<u32>,
}

impl ScreenShareSession {
    /// Open native capture/encoding and announce the screen track.
    pub async fn start(
        client: &Client,
        config: ss_video::PublisherConfig,
    ) -> Result<(Self, ss_video::CodecInfo), ScreenShareError> {
        Self::start_owned(client.screen_share_handle(), config).await
    }

    /// Start on a thread-local executor without retaining the host's client lock.
    pub async fn start_owned(
        client: ScreenShareHandle,
        config: ss_video::PublisherConfig,
    ) -> Result<(Self, ss_video::CodecInfo), ScreenShareError> {
        let (publisher, codec) = ss_video::Publisher::open(config).await?;
        let track = client
            .start_stream(ss_proto::v1::StartStream {
                kind: ss_proto::v1::TrackKind::Screen.into(),
                codec: Some(ss_proto::v1::Codec {
                    name: codec.name.clone(),
                    sample_rate: 0,
                    channels: 0,
                    description: codec.description.clone(),
                }),
            })
            .await?;
        let Some(connection) = client.media.quic() else {
            let _ = client.stop_stream(ss_proto::v1::StopStream {}).await;
            return Err(ScreenShareError::Reconnected);
        };
        let sender = VideoTrackSender::from_quinn(connection, track)?;
        Ok((
            Self {
                events: client.events.subscribe(),
                client,
                publisher,
                sender,
                writer: None,
                track,
                hint: ss_video::SubscriberHint::default(),
                clock_offset: None,
            },
            codec,
        ))
    }

    /// Capture, encode, and transmit the next available picture.
    pub async fn publish_next(&mut self) -> Result<(), ScreenShareError> {
        self.drain_events()?;
        let next = self.publisher.next().await;
        if matches!(next, Err(ss_video::Error::SourceEnded)) {
            if let Some(writer) = self.writer.take() {
                let _ = writer.finish();
            }
            let _ = self.client.stop_stream(ss_proto::v1::StopStream {}).await;
        }
        let Some(frames) = next? else {
            return Ok(());
        };
        for mut frame in frames {
            // Capture backends have independent epochs. Anchor video to the voice
            // capture clock once, then retain capture cadence through wraps.
            if self.clock_offset.is_none() {
                self.clock_offset = self
                    .client
                    .media
                    .capture_clock
                    .lock()
                    .and_then(crate::media::AudioClock::now)
                    .map(|t| t.wrapping_sub(frame.timestamp));
            }
            frame.timestamp = frame.timestamp.wrapping_add(self.clock_offset.unwrap_or(0));
            if frame.keyframe {
                if let Some(writer) = self.writer.take() {
                    writer.finish()?;
                }
                self.writer = Some(self.sender.begin_group(frame.timestamp).await?);
            }
            let writer = self
                .writer
                .as_mut()
                .ok_or(ss_video::Error::MissingKeyframe)?;
            writer
                .write_frame(&Frame {
                    timestamp: frame.timestamp,
                    keyframe: frame.keyframe,
                    payload: frame.payload,
                })
                .await?;
        }
        Ok(())
    }

    /// Server-assigned track id for UI state and event filtering.
    #[must_use]
    pub const fn track(&self) -> u32 {
        self.track
    }

    /// Latest aggregate viewer hint.
    #[must_use]
    pub const fn subscriber_hint(&self) -> ss_video::SubscriberHint {
        self.hint
    }

    /// Current native encoder diagnostics.
    #[must_use]
    pub fn stats(&self) -> ss_video::EncoderStats {
        self.publisher.stats()
    }

    /// Finish the active group and remove the published track.
    pub async fn stop(mut self) -> Result<(), ScreenShareError> {
        let finish = self.writer.take().map(GroupWriter::finish).transpose();
        let stop = self.client.stop_stream(ss_proto::v1::StopStream {}).await;
        finish?;
        stop?;
        Ok(())
    }

    fn drain_events(&mut self) -> Result<(), ScreenShareError> {
        if !self.client.valid() || !self.client.store.read().tracks.contains_key(&self.track) {
            return Err(ScreenShareError::Reconnected);
        }
        loop {
            match self.events.try_recv() {
                Ok(ClientEvent::KeyframeRequested { track }) if track == self.track => {
                    self.publisher.request_keyframe();
                }
                Ok(ClientEvent::SubscriberHint {
                    track,
                    max_height,
                    max_fps,
                    viewer_count,
                }) if track == self.track => {
                    self.hint = ss_video::SubscriberHint {
                        max_height,
                        max_fps,
                        viewer_count,
                    };
                }
                Ok(ClientEvent::Resynced | ClientEvent::Connection(_))
                | Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(ScreenShareError::Reconnected);
                }
                Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(broadcast::error::TryRecvError::Empty) => return Ok(()),
            }
        }
    }
}

/// Generation-bound control handle. It does not keep the connection worker alive.
#[derive(Clone)]
pub struct ScreenShareHandle {
    commands: mpsc::Sender<Command>,
    store: StateStore,
    events: broadcast::Sender<ClientEvent>,
    media: MediaLink,
    generation: u64,
}
impl Client {
    /// Capture a control handle before moving capture onto its local executor.
    #[must_use]
    pub fn screen_share_handle(&self) -> ScreenShareHandle {
        ScreenShareHandle {
            commands: self.commands.clone(),
            store: self.store.clone(),
            events: self.events.clone(),
            media: self.media.clone(),
            generation: self.store.read().generation,
        }
    }
}
impl ScreenShareHandle {
    /// Whether this handle still belongs to the live connection.
    #[must_use]
    pub fn valid(&self) -> bool {
        let state = self.store.read();
        state.generation == self.generation && state.connection == ConnectionState::Connected
    }
    async fn command(&self, message: Msg) -> Result<v1::Ack, crate::Error> {
        if !self.valid() {
            return Err(crate::Error::Disconnected);
        }
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command {
                message,
                generation: self.generation,
                replayable: false,
                reply,
            })
            .map_err(|_| crate::Error::Disconnected)?;
        response.await.map_err(|_| crate::Error::Disconnected)?
    }
    async fn start_stream(&self, input: v1::StartStream) -> Result<u32, crate::Error> {
        match self.command(Msg::StartStream(input)).await?.result {
            Some(v1::ack::Result::TrackId(track)) if track != 0 => Ok(track),
            _ => Err(crate::Error::Protocol("expected video track id")),
        }
    }
    /// Stop only this generation's publication; safe after reconnect.
    pub async fn stop_stream(&self, input: v1::StopStream) -> Result<(), crate::Error> {
        self.command(Msg::StopStream(input)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn welcome() -> v1::Welcome {
        v1::Welcome {
            user: Some(v1::User {
                id: ss_core::Id::generate().as_bytes().to_vec().into(),
                ..Default::default()
            }),
            snapshot: Some(v1::Snapshot::default()),
            ..Default::default()
        }
    }
    fn control() -> (ScreenShareHandle, mpsc::Receiver<Command>) {
        let store = StateStore::default();
        store.welcome(&welcome()).unwrap();
        let (commands, receive) = mpsc::channel(4);
        let (events, _) = broadcast::channel(4);
        (
            ScreenShareHandle {
                commands,
                generation: store.read().generation,
                store: store.clone(),
                events,
                media: MediaLink::new(),
            },
            receive,
        )
    }
    #[tokio::test]
    async fn owned_control_correlates_track_and_never_replays_publication() {
        let (handle, mut receive) = control();
        let request = handle.start_stream(v1::StartStream::default());
        let reply = async {
            let command = receive.recv().await.unwrap();
            assert!(!command.replayable);
            assert_eq!(command.generation, handle.generation);
            assert!(matches!(command.message, Msg::StartStream(_)));
            command
                .reply
                .send(Ok(v1::Ack {
                    result: Some(v1::ack::Result::TrackId(42)),
                    ..Default::default()
                }))
                .unwrap();
        };
        let (track, ()) = tokio::join!(request, reply);
        assert_eq!(track.unwrap(), 42);
    }
    #[tokio::test]
    async fn stale_capture_cannot_stop_a_new_generations_share() {
        let (handle, mut receive) = control();
        handle.store.welcome(&welcome()).unwrap();
        assert!(!handle.valid());
        assert!(matches!(
            handle.stop_stream(v1::StopStream {}).await,
            Err(crate::Error::Disconnected)
        ));
        assert!(receive.try_recv().is_err());
    }
}
