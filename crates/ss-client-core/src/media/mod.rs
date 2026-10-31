//! Byte-level screen/video transport. Call StartStream/SubscribeTrack on the
//! control stream first; capture, decoding, and rendering belong above this API.
use ss_net::media::{DatagramHeader, MAX_TRACK_ID, StreamHeader, stream};
pub use stream::{Error, Frame, GroupReader, GroupWriter};

/// Opens GOP streams for a server-assigned video track.
pub struct VideoTrackSender {
    connection: quinn::Connection,
    track: u32,
    group: u32,
    seq: u16,
}
impl VideoTrackSender {
    /// Bind to the track id returned by StartStream. No media is sent yet.
    pub fn new(connection: &ss_net::Connection, track: u32) -> Result<Self, Error> {
        if track == 0 || track > MAX_TRACK_ID {
            return Err(ss_net::media::MediaError::TrackId.into());
        }
        Ok(Self {
            connection: connection.quic().clone(),
            track,
            group: 0,
            seq: 0,
        })
    }
    /// Start a keyframe group. Reset/drop the previous unfinished writer when
    /// a new keyframe supersedes it. Sequence and group ids wrap on the wire.
    pub async fn begin_group(&mut self, timestamp: u32) -> Result<GroupWriter, Error> {
        let header = StreamHeader {
            media: DatagramHeader {
                version: 0,
                kind: 1,
                flags: 0,
                track_id: self.track,
                seq: self.seq,
                timestamp,
            },
            group_id: self.group,
        };
        self.seq = self.seq.wrapping_add(1);
        self.group = self.group.wrapping_add(1);
        GroupWriter::open(&self.connection, header).await
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
