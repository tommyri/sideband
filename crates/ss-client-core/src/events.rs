//! Notifications describe changes already applied to the state store.

use ss_core::Id;
use ss_proto::v1;
use std::time::Duration;

/// Lifecycle of one server connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionState {
    /// No live connection; an explicit connect is needed.
    #[default]
    Disconnected,
    /// The initial TLS/authentication exchange is running.
    Connecting,
    /// Authenticated, with a complete snapshot applied.
    Connected,
    /// Waiting before the next session-resume attempt.
    Reconnecting {
        /// Consecutive failed attempts, starting at one.
        attempt: u32,
        /// Delay before trying again.
        delay: Duration,
    },
    /// Authentication, certificate approval, or a protocol failure needs the host.
    Failed(String),
}

/// Subscribe before reading the state snapshot. On broadcast lag, read the
/// snapshot again: notifications are invalidations, not the source of truth.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Full snapshot replaced (including after reconnect).
    Resynced,
    /// Channel tree, DMs, or resolved permissions changed.
    ChannelsChanged,
    /// Cached messages changed.
    MessagesChanged {
        /// Affected channel.
        channel: Id,
    },
    /// Voice memberships or tracks changed.
    VoiceChanged,
    /// Profiles, roles, or presence changed.
    MembersChanged,
    /// Own read cursors changed.
    ReadStatesChanged,
    /// Invite list changed.
    InvitesChanged,
    /// Ephemeral typing indication.
    Typing {
        /// Destination.
        channel: Id,
        /// Typing member.
        user: Id,
    },
    /// Lifecycle changed.
    Connection(ConnectionState),
    /// Server informational text.
    Notice(String),
    /// The server removed this session; automatic reconnect stops.
    Removed {
        /// True for a ban.
        banned: bool,
        /// Server-supplied reason.
        reason: String,
    },
    /// An unsolicited server error.
    ServerError(v1::Error),
}
