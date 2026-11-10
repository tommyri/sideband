//! Unread conversations, derived from the mirrored server state.
//!
//! Hosts show these in channel lists, DM lists and server badges; deriving
//! them in one place keeps every view in agreement.

use crate::State;
use bytes::Bytes;
use ss_core::Id;

/// The newest message known for a conversation: the later of the newest
/// cached message and the server's `last_message_id`. Message ids are ULIDs,
/// so byte order is time order.
#[must_use]
pub fn newest_message(state: &State, channel: Id) -> Option<&Bytes> {
    let cached = state
        .messages
        .get(&channel)
        .and_then(|cache| cache.ordered().last().map(|m| &m.id));
    let known = state
        .channels
        .get(&channel)
        .or_else(|| state.direct_messages.get(&channel))
        .and_then(|c| c.last_message_id.as_ref());
    cached.max(known)
}

/// Whether a conversation has a message newer than the user's read cursor.
#[must_use]
pub fn is_unread(state: &State, channel: Id) -> bool {
    newest_message(state, channel).is_some_and(|newest| {
        state
            .read_states
            .get(&channel)
            .is_none_or(|read| read.last_read_message_id < *newest)
    })
}

/// Guild channels plus the direct conversations the user belongs to that
/// have unread messages.
#[must_use]
pub fn unread_conversations(state: &State) -> usize {
    let own = state.own_user.as_ref().map(|u| &u.id);
    let direct = state
        .direct_messages
        .iter()
        .filter(|(_, dm)| own.is_some_and(|own| dm.member_ids.contains(own)))
        .map(|(id, _)| id);
    state
        .channels
        .keys()
        .chain(direct)
        .filter(|id| is_unread(state, **id))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1;

    fn bytes(id: Id) -> Bytes {
        id.as_bytes().to_vec().into()
    }

    /// Message ids in a fixed order; generated ids within one millisecond
    /// are not ordered.
    fn message(n: u8) -> Bytes {
        vec![n; 16].into()
    }

    fn state_with(channel: Id, last: Option<Bytes>) -> State {
        let mut state = State::default();
        state.channels.insert(
            channel,
            v1::Channel {
                id: bytes(channel),
                last_message_id: last,
                ..Default::default()
            },
        );
        state
    }

    #[test]
    fn a_channel_is_unread_until_the_cursor_reaches_its_newest_message() {
        let channel = Id::generate();
        let (older, newer) = (message(1), message(2));
        assert!(!is_unread(&state_with(channel, None), channel));
        let mut state = state_with(channel, Some(newer.clone()));
        assert!(is_unread(&state, channel));
        state.read_states.insert(
            channel,
            v1::ReadState {
                last_read_message_id: older,
                ..Default::default()
            },
        );
        assert!(is_unread(&state, channel));
        state
            .read_states
            .get_mut(&channel)
            .unwrap()
            .last_read_message_id = newer;
        assert!(!is_unread(&state, channel));
        assert_eq!(unread_conversations(&state), 0);
    }

    #[test]
    fn counts_guild_channels_and_only_own_direct_messages() {
        let channel = Id::generate();
        let mut state = state_with(channel, Some(message(1)));
        let own = Id::generate();
        state.own_user = Some(v1::User {
            id: bytes(own),
            ..Default::default()
        });
        for members in [vec![bytes(own)], vec![bytes(Id::generate())]] {
            let dm = Id::generate();
            state.direct_messages.insert(
                dm,
                v1::Channel {
                    id: bytes(dm),
                    member_ids: members,
                    last_message_id: Some(message(1)),
                    ..Default::default()
                },
            );
        }
        assert_eq!(unread_conversations(&state), 2);
    }
}
