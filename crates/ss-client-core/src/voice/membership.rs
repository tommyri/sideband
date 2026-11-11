//! Where a call's seat is, from the call's own state.
//!
//! Every host polls [`super::VoiceSession::reconcile`] at UI cadence and acts
//! on the answer, so a moderator move, a lost seat and a retrying connection
//! behave the same in the desktop, the CLI and tests.

use ss_core::Id;

use crate::{ConnectionState, State, state::id};

/// What a call's seat is doing, from [`super::VoiceSession::reconcile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// Seated in the session's channel on a live connection.
    Seated,
    /// The connection is retrying. Push-to-talk has been released, so a key
    /// held through the outage does not transmit after it. The core rejoins
    /// the seat when the connection is back.
    Retrying,
    /// A moderator moved the seat to `channel`. Pass the session to
    /// [`super::VoiceSession::follow`], which restarts it muted there.
    Follow {
        /// The channel the seat is in now.
        channel: Id,
    },
    /// The seat is gone (left on another device, kicked, lost access), or the
    /// connection is down for good. Drop the session.
    Ended,
}

/// Reads the seat of the tracked connection. A reconnect that did not resume
/// the lease has a new connection id; `tracked` follows it.
pub(super) fn membership(state: &State, tracked: &mut Id, channel: Id) -> Membership {
    match state.connection {
        ConnectionState::Connected => {}
        ConnectionState::Reconnecting { .. } => return Membership::Retrying,
        ConnectionState::Disconnected
        | ConnectionState::Connecting
        | ConnectionState::Failed(_) => {
            return Membership::Ended;
        }
    }
    // Older servers do not name the connection; keep the one from the join.
    if let Some(conn) = state.own_conn {
        *tracked = conn;
    }
    let seat = state
        .voice_states
        .get(tracked)
        .and_then(|voice| voice.channel_id.as_ref())
        .and_then(|seat| id(seat).ok());
    match seat {
        None => Membership::Ended,
        Some(seat) if seat == channel => Membership::Seated,
        Some(seat) => Membership::Follow { channel: seat },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ss_proto::{v1, validate::id_to_bytes};

    use super::*;

    fn seated(conn: Id, channel: Option<Id>) -> State {
        let mut state = State {
            connection: ConnectionState::Connected,
            own_conn: Some(conn),
            ..State::default()
        };
        state.voice_states.insert(
            conn,
            v1::VoiceState {
                conn_id: id_to_bytes(conn),
                channel_id: channel.map(id_to_bytes),
                ..Default::default()
            },
        );
        state
    }

    #[test]
    fn the_seat_in_the_sessions_channel_is_seated_and_elsewhere_is_followed() {
        let (conn, channel, other) = (Id::generate(), Id::generate(), Id::generate());
        let mut tracked = conn;
        let state = seated(conn, Some(channel));
        assert_eq!(
            membership(&state, &mut tracked, channel),
            Membership::Seated
        );
        assert_eq!(
            membership(&state, &mut tracked, other),
            Membership::Follow { channel }
        );
    }

    #[test]
    fn a_missing_seat_or_a_dead_connection_ends_the_call() {
        let (conn, channel) = (Id::generate(), Id::generate());
        let mut tracked = conn;
        let left = seated(conn, None);
        assert_eq!(membership(&left, &mut tracked, channel), Membership::Ended);
        let mut gone = seated(conn, Some(channel));
        gone.voice_states.clear();
        assert_eq!(membership(&gone, &mut tracked, channel), Membership::Ended);
        for connection in [
            ConnectionState::Disconnected,
            ConnectionState::Connecting,
            ConnectionState::Failed("banned".into()),
        ] {
            let state = State {
                connection,
                ..seated(conn, Some(channel))
            };
            assert_eq!(membership(&state, &mut tracked, channel), Membership::Ended);
        }
    }

    /// While retrying, the snapshot may already carry the new connection's
    /// id before its seat is back. That is not an ended call.
    #[test]
    fn retrying_is_reported_before_the_seat_is_read() {
        let (conn, channel) = (Id::generate(), Id::generate());
        let mut tracked = conn;
        let state = State {
            connection: ConnectionState::Reconnecting {
                attempt: 1,
                delay: Duration::from_secs(2),
            },
            own_conn: Some(Id::generate()),
            ..State::default()
        };
        assert_eq!(
            membership(&state, &mut tracked, channel),
            Membership::Retrying
        );
        assert_eq!(tracked, conn);
    }

    #[test]
    fn a_new_connection_id_is_tracked_and_keeps_the_seat() {
        let (old, new, channel) = (Id::generate(), Id::generate(), Id::generate());
        let mut tracked = old;
        let state = seated(new, Some(channel));
        assert_eq!(
            membership(&state, &mut tracked, channel),
            Membership::Seated
        );
        assert_eq!(tracked, new);
    }

    /// Older servers send no connection id: the call keeps the one it joined
    /// with, and a seat on another connection is not its own.
    #[test]
    fn without_a_connection_id_the_joined_connection_is_kept() {
        let (conn, other, channel) = (Id::generate(), Id::generate(), Id::generate());
        let mut tracked = conn;
        let mut state = State {
            own_conn: None,
            ..seated(conn, Some(channel))
        };
        assert_eq!(
            membership(&state, &mut tracked, channel),
            Membership::Seated
        );
        state.voice_states = seated(other, Some(channel)).voice_states;
        assert_eq!(membership(&state, &mut tracked, channel), Membership::Ended);
        assert_eq!(tracked, conn);
    }
}
