//! Capture/encode integration with Sideband video groups.

use tokio::sync::broadcast;

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
/// pull-driven because ScreenCaptureKit frames are main-thread bound on macOS;
/// the desktop shell can own it on its local executor.
pub struct ScreenShareSession<'a> {
    client: &'a Client,
    publisher: ss_video::Publisher,
    sender: VideoTrackSender,
    writer: Option<GroupWriter>,
    events: broadcast::Receiver<ClientEvent>,
    track: u32,
    hint: ss_video::SubscriberHint,
    clock_offset: Option<u32>,
}

impl<'a> ScreenShareSession<'a> {
    /// Open native capture/encoding and announce the screen track.
    pub async fn start(
        client: &'a Client,
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
                client,
                publisher,
                sender,
                writer: None,
                events: client.subscribe(),
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
