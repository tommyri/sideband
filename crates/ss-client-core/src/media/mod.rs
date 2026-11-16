//! Byte-level screen/video transport. Call StartStream/SubscribeTrack on the
//! control stream first; capture, decoding, and rendering belong above this API.
mod switch;
pub(crate) mod viewer;
use ss_net::media::{DatagramHeader, LOW_RENDITION, MAX_TRACK_ID, StreamHeader, stream};
pub use stream::{Error, Frame, GroupReader, GroupWriter};
pub use switch::{Step, Switcher};
pub use viewer::{AudioClock, VideoSubscription};
#[cfg(feature = "screen-share")]
mod sender;
#[cfg(feature = "screen-share")]
pub use sender::{
    ScreenShareError, ScreenShareHandle, ScreenShareSession, start_stream, viewer_report,
};

/// Opens GOP streams for a server-assigned video track.
pub struct VideoTrackSender {
    connection: quinn::Connection,
    track: u32,
    /// Group and sequence ids count per rendition, so each rendition's run of
    /// groups is gap-free on its own.
    group: [u32; 2],
    seq: [u16; 2],
}
impl VideoTrackSender {
    /// Bind to the track id returned by StartStream. No media is sent yet.
    pub fn new(connection: &ss_net::Connection, track: u32) -> Result<Self, Error> {
        Self::from_quinn(connection.quic().clone(), track)
    }

    fn from_quinn(connection: quinn::Connection, track: u32) -> Result<Self, Error> {
        if track == 0 || track > MAX_TRACK_ID {
            return Err(ss_net::media::MediaError::TrackId.into());
        }
        Ok(Self {
            connection,
            track,
            group: [0; 2],
            seq: [0; 2],
        })
    }
    /// Start a keyframe group of the full rendition.
    pub async fn begin_group(&mut self, timestamp: u32) -> Result<GroupWriter, Error> {
        self.begin_rendition_group(0, timestamp).await
    }
    /// Start a keyframe group of `rendition` (0 full, 1 low). Reset/drop the
    /// previous unfinished writer of that rendition when a new keyframe
    /// supersedes it. Sequence and group ids wrap on the wire.
    pub async fn begin_rendition_group(
        &mut self,
        rendition: u8,
        timestamp: u32,
    ) -> Result<GroupWriter, Error> {
        let index = usize::from(rendition.min(1));
        let header = StreamHeader {
            media: DatagramHeader {
                version: 0,
                kind: 1,
                flags: if index == 1 { LOW_RENDITION } else { 0 },
                track_id: self.track,
                seq: self.seq[index],
                timestamp,
            },
            group_id: self.group[index],
        };
        self.seq[index] = self.seq[index].wrapping_add(1);
        self.group[index] = self.group[index].wrapping_add(1);
        GroupWriter::open(&self.connection, header).await
    }
}

/// Writes each rendition's frames into its own run of groups: a keyframe
/// finishes that rendition's previous group and opens the next one.
pub struct RenditionWriter {
    sender: VideoTrackSender,
    writers: [Option<GroupWriter>; 2],
    written: u64,
}
impl RenditionWriter {
    /// Wrap a track sender. Nothing is sent until the first keyframe.
    #[must_use]
    pub const fn new(sender: VideoTrackSender) -> Self {
        Self {
            sender,
            writers: [None, None],
            written: 0,
        }
    }
    /// Write one access unit of `rendition`. A delta frame before that
    /// rendition's first keyframe is skipped.
    pub async fn write(&mut self, rendition: u8, frame: &Frame) -> Result<(), Error> {
        let index = usize::from(rendition.min(1));
        if frame.keyframe {
            if let Some(writer) = self.writers[index].take() {
                writer.finish()?;
            }
            self.writers[index] = Some(
                self.sender
                    .begin_rendition_group(rendition, frame.timestamp)
                    .await?,
            );
            self.written += 18;
        }
        let Some(writer) = self.writers[index].as_mut() else {
            return Ok(());
        };
        writer.write_frame(frame).await?;
        self.written += 9 + u64::try_from(frame.payload.len()).unwrap_or(u64::MAX);
        Ok(())
    }
    /// End `rendition`'s current group, for example when its encoder stops.
    pub fn finish(&mut self, rendition: u8) -> Result<(), Error> {
        self.writers[usize::from(rendition.min(1))]
            .take()
            .map(GroupWriter::finish)
            .transpose()
            .map(|_| ())
    }
    /// End every open group.
    pub fn finish_all(&mut self) -> Result<(), Error> {
        let full = self.finish(0);
        let low = self.finish(1);
        full.and(low)
    }
    /// Video bytes written so far, prefixes included, for rate control.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }
}
/// Single owner of a connection's incoming video streams; demux by header.track_id.
/// Do not create competing receivers per track: uni streams share one accept queue.
pub struct VideoTrackReceiver {
    connection: quinn::Connection,
}
impl VideoTrackReceiver {
    /// Construct for an authenticated connection after subscribing to tracks.
    pub fn new(connection: &ss_net::Connection) -> Self {
        Self {
            connection: connection.quic().clone(),
        }
    }
    /// Accept the next group. A group reset means skip to the next keyframe.
    pub async fn next_group(&mut self) -> Result<GroupReader, Error> {
        GroupReader::accept(&self.connection).await
    }
}
