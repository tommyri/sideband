//! Capture/encode integration with Sideband video groups.

use crate::{
    ConnectionState, StateStore,
    connection::{Command, MediaLink},
};
use ss_proto::v1::{self, client_envelope::Msg};
use tokio::sync::{broadcast, mpsc, oneshot};

use super::{Frame, RenditionWriter, VideoTrackSender};
use crate::{Client, ClientEvent};
use ss_video::rate::{LinkSample, RATE_TICK, RateController, ViewerReport};
use std::time::Instant;

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

/// One live screen share: capture, native encoders, and QUIC group writer.
///
/// The host repeatedly calls [`publish_next`](Self::publish_next). This is
/// pull-driven because native video frames are thread-confined; the desktop shell
/// owns capture and encoding together on a dedicated local executor.
pub struct ScreenShareSession {
    client: ScreenShareHandle,
    publisher: ss_video::Publisher,
    writer: RenditionWriter,
    connection: quinn::Connection,
    events: broadcast::Receiver<ClientEvent>,
    track: u32,
    hint: ss_video::SubscriberHint,
    clock_offset: Option<u32>,
    rate: RateController,
    next_rate: Instant,
    low_viewers: u32,
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
        let preset = config.preset;
        let (mut publisher, codec) = ss_video::Publisher::open(config).await?;
        let track = client
            .start_stream(start_stream(&codec, publisher.encoders().renditions()))
            .await?;
        let Some(connection) = client.media.quic() else {
            let _ = client.stop_stream(ss_proto::v1::StopStream {}).await;
            return Err(ScreenShareError::Reconnected);
        };
        let sender = VideoTrackSender::from_quinn(connection.clone(), track)?;
        Ok((
            Self {
                events: client.events.subscribe(),
                client,
                publisher,
                writer: RenditionWriter::new(sender),
                connection,
                track,
                hint: ss_video::SubscriberHint::default(),
                clock_offset: None,
                rate: RateController::new(preset.bitrate()),
                next_rate: Instant::now() + RATE_TICK,
                low_viewers: 0,
            },
            codec,
        ))
    }

    /// Capture, encode, and transmit the next available picture.
    pub async fn publish_next(&mut self) -> Result<(), ScreenShareError> {
        self.drain_events()?;
        self.adapt_rate().await?;
        let next = self.publisher.next().await;
        if matches!(next, Err(ss_video::Error::SourceEnded)) {
            let _ = self.writer.finish_all();
            let _ = self.client.stop_stream(ss_proto::v1::StopStream {}).await;
        }
        let Some(frames) = next? else {
            return Ok(());
        };
        for frame in frames {
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
            let timestamp = frame.timestamp.wrapping_add(self.clock_offset.unwrap_or(0));
            self.writer
                .write(
                    frame.rendition,
                    &Frame {
                        timestamp,
                        keyframe: frame.keyframe,
                        payload: frame.payload,
                    },
                )
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

    /// Viewers on the low rendition in the server's latest report.
    #[must_use]
    pub const fn low_viewers(&self) -> u32 {
        self.low_viewers
    }

    /// Current native encoder diagnostics, including the low rendition's state.
    #[must_use]
    pub fn stats(&self) -> ss_video::EncoderStats {
        self.publisher.stats()
    }

    /// Finish the active groups and remove the published track.
    pub async fn stop(mut self) -> Result<(), ScreenShareError> {
        let finish = self.writer.finish_all();
        let stop = self.client.stop_stream(ss_proto::v1::StopStream {}).await;
        finish?;
        stop?;
        Ok(())
    }

    /// Feed the sender-wide controller and retune the full rendition.
    async fn adapt_rate(&mut self) -> Result<(), ScreenShareError> {
        let now = Instant::now();
        if now < self.next_rate {
            return Ok(());
        }
        self.next_rate = now + RATE_TICK;
        let stats = self.connection.stats();
        let sample = LinkSample {
            written: self.writer.written(),
            delivered: stats.udp_tx.bytes.saturating_sub(stats.path.lost_bytes),
            rtt: self.connection.rtt(),
        };
        if let Some(target) = self.rate.update(now, sample) {
            tracing::debug!(target, reason = ?self.rate.reason(), "screen-share bitrate");
            self.publisher.encoders().set_bitrate(target).await?;
        }
        Ok(())
    }

    fn drain_events(&mut self) -> Result<(), ScreenShareError> {
        if !self.client.valid() || !self.client.store.read().tracks.contains_key(&self.track) {
            return Err(ScreenShareError::Reconnected);
        }
        loop {
            match self.events.try_recv() {
                Ok(ClientEvent::KeyframeRequested { track, rendition }) if track == self.track => {
                    self.publisher
                        .encoders()
                        .request_keyframe(u8::try_from(rendition.min(1)).unwrap_or(0));
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
                Ok(ClientEvent::SubscriberStats { track, renditions }) if track == self.track => {
                    self.subscriber_stats(&renditions)?;
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

    fn subscriber_stats(
        &mut self,
        renditions: &[v1::RenditionStats],
    ) -> Result<(), ScreenShareError> {
        let report = viewer_report(renditions, self.publisher.encoders().renditions().len());
        self.rate.report(report);
        self.low_viewers = renditions.get(1).map_or(0, |low| low.subscribers);
        let wanted = self.low_viewers > 0;
        self.publisher.encoders().set_low_wanted(wanted);
        if !wanted {
            // The low rendition stops between groups, not inside one.
            self.writer.finish(1)?;
        }
        Ok(())
    }
}

/// `StartStream` for the announced renditions; one rendition sends none.
#[must_use]
pub fn start_stream(
    codec: &ss_video::CodecInfo,
    renditions: &[ss_video::RenditionInfo],
) -> v1::StartStream {
    v1::StartStream {
        kind: v1::TrackKind::Screen.into(),
        codec: Some(wire_codec(codec)),
        renditions: if renditions.len() < 2 {
            Vec::new()
        } else {
            renditions
                .iter()
                .map(|r| v1::Rendition {
                    index: u32::from(r.index),
                    width: r.width,
                    height: r.height,
                    fps: r.fps,
                    bitrate: r.bitrate,
                    codec: Some(wire_codec(&r.codec)),
                })
                .collect()
        },
    }
}

fn wire_codec(codec: &ss_video::CodecInfo) -> v1::Codec {
    v1::Codec {
        name: codec.name.clone(),
        sample_rate: 0,
        channels: 0,
        description: codec.description.clone(),
    }
}

/// Reduce a server `SubscriberStats` report to the rate controller's input.
#[must_use]
pub fn viewer_report(renditions: &[v1::RenditionStats], announced: usize) -> ViewerReport {
    let full = renditions.first().copied().unwrap_or_default();
    ViewerReport {
        full_subscribers: full.subscribers,
        full_congested: full.congested.max(u32::from(full.resets > 0)),
        low_available: announced >= 2,
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
    fn info(index: u8, height: u32) -> ss_video::RenditionInfo {
        ss_video::RenditionInfo {
            index,
            width: height * 16 / 9,
            height,
            fps: 30,
            bitrate: 1,
            codec: ss_video::CodecInfo {
                name: format!("avc3.{height}"),
                description: bytes::Bytes::new(),
            },
        }
    }

    #[test]
    fn one_rendition_announces_none_and_two_announce_both() {
        let one = [info(0, 1080)];
        let start = start_stream(&one[0].codec, &one);
        assert_eq!(start.renditions, []);
        assert_eq!(start.codec.unwrap().name, "avc3.1080");
        let two = [info(0, 1080), info(1, 360)];
        let start = start_stream(&two[0].codec, &two);
        let heights: Vec<_> = start
            .renditions
            .iter()
            .map(|r| (r.index, r.height))
            .collect();
        assert_eq!(heights, [(0, 1080), (1, 360)]);
        assert_eq!(start.renditions[1].codec.as_ref().unwrap().name, "avc3.360");
    }

    #[test]
    fn viewer_report_counts_resets_as_congestion() {
        let stats = |congested, resets| {
            vec![v1::RenditionStats {
                index: 0,
                subscribers: 2,
                congested,
                resets,
                ..Default::default()
            }]
        };
        let report = viewer_report(&stats(0, 3), 2);
        assert_eq!((report.full_subscribers, report.full_congested), (2, 1));
        assert!(report.low_available);
        let report = viewer_report(&stats(2, 0), 1);
        assert_eq!(report.full_congested, 2);
        assert!(!report.low_available);
        assert_eq!(viewer_report(&[], 2).full_subscribers, 0);
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
