//! The signed-in user's server-wide authority, derived from the mirrored
//! roles. Hosts use it to decide which actions to offer; the server checks
//! every command again and its answer is final.

use crate::{State, v1};
use ss_core::{Id, Permissions};

/// Roles held by `user`, the default role included.
#[must_use]
pub fn roles(state: &State, user: Id) -> Vec<&v1::Role> {
    let member = state.members.get(&user);
    state
        .roles
        .values()
        .filter(|role| role.is_default || member.is_some_and(|m| m.roles.contains(&role.id)))
        .collect()
}

/// What the signed-in user may do to one other member.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per moderation action"
)]
pub struct ModerationRights {
    /// Remove the member from the server.
    pub kick: bool,
    /// Ban the member.
    pub ban: bool,
    /// Server-mute the member in voice.
    pub mute: bool,
    /// Move the member between voice channels.
    pub move_members: bool,
}

/// Server-wide permissions and rank of the signed-in user.
#[derive(Clone, Copy, Debug)]
pub struct Authority {
    own: Id,
    permissions: Permissions,
    highest: i32,
}

impl Authority {
    /// The signed-in user's authority, or `None` before Welcome.
    #[must_use]
    pub fn own(state: &State) -> Option<Self> {
        let own = Id::try_from(state.own_user.as_ref()?.id.as_ref()).ok()?;
        let roles = roles(state, own);
        Some(Self {
            own,
            permissions: roles.iter().fold(Permissions::empty(), |p, r| {
                p | Permissions::from_bits_truncate(r.permissions)
            }),
            highest: roles.iter().map(|r| r.position).max().unwrap_or(i32::MIN),
        })
    }

    /// The signed-in user's id.
    pub fn user(&self) -> Id {
        self.own
    }

    /// Whether the user holds `permission` server-wide. Administrators hold
    /// every permission.
    #[must_use]
    pub fn allows(&self, permission: Permissions) -> bool {
        self.permissions.contains(Permissions::ADMINISTRATOR)
            || self.permissions.contains(permission)
    }

    /// Whether the user may act on `target`: never on themselves; otherwise
    /// as an administrator or with a higher top role than the target's.
    #[must_use]
    pub fn outranks(&self, state: &State, target: Id) -> bool {
        let top = roles(state, target)
            .iter()
            .map(|r| r.position)
            .max()
            .unwrap_or(i32::MIN);
        target != self.own
            && (self.permissions.contains(Permissions::ADMINISTRATOR) || self.highest > top)
    }

    /// The moderation actions the user may take on `target`.
    #[must_use]
    pub fn rights_over(&self, state: &State, target: Id) -> ModerationRights {
        let can = |permission| self.outranks(state, target) && self.allows(permission);
        ModerationRights {
            kick: can(Permissions::KICK_MEMBERS),
            ban: can(Permissions::BAN_MEMBERS),
            mute: can(Permissions::MUTE_MEMBERS),
            move_members: can(Permissions::MOVE_MEMBERS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(id: Id) -> bytes::Bytes {
        id.as_bytes().to_vec().into()
    }

    fn state(own: Id) -> State {
        let mut state = State {
            own_user: Some(v1::User {
                id: bytes(own),
                ..Default::default()
            }),
            ..Default::default()
        };
        state.roles.insert(
            Id::generate(),
            v1::Role {
                id: bytes(Id::generate()),
                is_default: true,
                position: 0,
                ..Default::default()
            },
        );
        state
    }

    fn grant(state: &mut State, user: Id, permissions: Permissions, position: i32) {
        let role = Id::generate();
        state.roles.insert(
            role,
            v1::Role {
                id: bytes(role),
                permissions: permissions.bits(),
                position,
                ..Default::default()
            },
        );
        state
            .members
            .entry(user)
            .or_default()
            .roles
            .push(bytes(role));
    }

    #[test]
    fn moderation_needs_the_permission_and_a_higher_role() {
        let (own, peer, senior) = (Id::generate(), Id::generate(), Id::generate());
        let mut state = state(own);
        grant(&mut state, own, Permissions::KICK_MEMBERS, 5);
        grant(&mut state, senior, Permissions::empty(), 9);
        let authority = Authority::own(&state).unwrap();
        assert_eq!(authority.user(), own);
        assert_eq!(
            authority.rights_over(&state, peer),
            ModerationRights {
                kick: true,
                ..Default::default()
            }
        );
        assert_eq!(
            authority.rights_over(&state, senior),
            ModerationRights::default()
        );
        assert_eq!(
            authority.rights_over(&state, own),
            ModerationRights::default()
        );
        assert!(!authority.allows(Permissions::MANAGE_SERVER));
    }

    #[test]
    fn administrators_act_on_anyone_but_themselves() {
        let (own, senior) = (Id::generate(), Id::generate());
        let mut state = state(own);
        grant(&mut state, own, Permissions::ADMINISTRATOR, 1);
        grant(&mut state, senior, Permissions::empty(), 9);
        let authority = Authority::own(&state).unwrap();
        assert!(authority.allows(Permissions::MANAGE_SERVER));
        assert!(authority.rights_over(&state, senior).ban);
        assert!(!authority.outranks(&state, own));
        assert!(Authority::own(&State::default()).is_none());
    }
}
