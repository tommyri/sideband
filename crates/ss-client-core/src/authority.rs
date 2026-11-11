//! What the signed-in user may do, derived from the mirrored roles and the
//! server's resolved channel permissions. Hosts use it to decide which
//! actions to offer; the server checks every command again and its answer is
//! final. The rules follow the server's (`ss-server` `perms` and handlers).

use crate::{ConnectionState, State, v1};
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

/// Known bits of a role mask. Bits from a newer server grant nothing here.
fn known(mask: u64) -> Permissions {
    Permissions::from_bits_truncate(mask)
}

/// Highest position among `user`'s roles.
fn top(state: &State, user: Id) -> i32 {
    roles(state, user)
        .iter()
        .map(|r| r.position)
        .max()
        .unwrap_or(i32::MIN)
}

/// Whether `user` holds `ADMINISTRATOR` through a role.
fn administrator(state: &State, user: Id) -> bool {
    roles(state, user)
        .iter()
        .any(|r| known(r.permissions).contains(Permissions::ADMINISTRATOR))
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
            permissions: roles
                .iter()
                .fold(Permissions::empty(), |p, r| p | known(r.permissions)),
            highest: top(state, own),
        })
    }

    /// The signed-in user's id.
    pub fn user(&self) -> Id {
        self.own
    }

    /// Whether the user holds `ADMINISTRATOR` through a role.
    #[must_use]
    pub fn is_administrator(&self) -> bool {
        self.permissions.contains(Permissions::ADMINISTRATOR)
    }

    /// Whether the user holds `permission` server-wide. Administrators hold
    /// every permission.
    #[must_use]
    pub fn allows(&self, permission: Permissions) -> bool {
        self.is_administrator() || self.permissions.contains(permission)
    }

    /// Whether the user may create, edit, move and delete channels.
    #[must_use]
    pub fn can_manage_channels(&self) -> bool {
        self.allows(Permissions::MANAGE_CHANNELS)
    }

    /// The server's `can_act_on`: an administrator acts on anyone, themselves
    /// included; nobody else acts on an administrator; otherwise the user's
    /// top role must be strictly higher than the target's.
    fn acts_on(&self, state: &State, target: Id) -> bool {
        self.is_administrator()
            || (!administrator(state, target) && self.highest > top(state, target))
    }

    /// Whether the user may moderate `target`: never themselves, otherwise
    /// as [`Self::acts_on`].
    #[must_use]
    pub fn outranks(&self, state: &State, target: Id) -> bool {
        target != self.own && self.acts_on(state, target)
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

    /// Whether `role` is within the user's reach: an administrator reaches
    /// every role; anyone else only roles strictly below their top role that
    /// do not carry `ADMINISTRATOR`. Editing, deleting and assigning a role
    /// also need `MANAGE_ROLES`, and a channel override `MANAGE_CHANNELS`.
    #[must_use]
    pub fn can_edit_role(&self, role: &v1::Role) -> bool {
        self.is_administrator()
            || (role.position < self.highest
                && !known(role.permissions).contains(Permissions::ADMINISTRATOR))
    }

    /// Whether the user may change `member`'s roles: `MANAGE_ROLES`, and
    /// [`Self::acts_on`] the member. Each role added or removed must also
    /// pass [`Self::can_edit_role`].
    #[must_use]
    pub fn can_set_roles_of(&self, state: &State, member: Id) -> bool {
        self.allows(Permissions::MANAGE_ROLES) && self.acts_on(state, member)
    }

    /// Position for a new role: the bottom for an administrator, otherwise
    /// just below the user's top role. `None` when there is no room below it.
    #[must_use]
    pub fn new_role_position(&self) -> Option<i32> {
        if self.is_administrator() {
            Some(0)
        } else {
            self.highest.checked_sub(1)
        }
    }
}

/// Whether the user holds every bit of `permissions` in `channel`, as the
/// server resolved them (overrides and administrator included). False for an
/// unknown channel or one without a resolved mask.
#[must_use]
pub fn holds_in(state: &State, channel: Id, permissions: Permissions) -> bool {
    state
        .permissions(channel)
        .is_some_and(|held| held.contains(permissions))
}

/// What the signed-in user may do in one text channel they can see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChatRights {
    /// Send messages.
    pub send: bool,
    /// Attach files to a message. The server checks `SEND_MESSAGES` first.
    pub attach: bool,
    /// Delete other members' messages.
    pub manage: bool,
}

impl ChatRights {
    /// The user's rights in `channel`, or `None` when they cannot see it.
    #[must_use]
    pub fn of(state: &State, channel: Id) -> Option<Self> {
        let held = state.permissions(channel)?;
        if !held.contains(Permissions::VIEW_CHANNEL) {
            return None;
        }
        let send = held.contains(Permissions::SEND_MESSAGES);
        Some(Self {
            send,
            attach: send && held.contains(Permissions::ATTACH_FILES),
            manage: held.contains(Permissions::MANAGE_MESSAGES),
        })
    }
}

/// Whether voice `connection` may share its screen in `channel`: the client
/// is connected, the connection is seated in that channel, and the user holds
/// `VIEW_CHANNEL` and `STREAM` there. `CONNECT` is not needed: a member
/// moved in by a moderator keeps the seat without it.
#[must_use]
pub fn can_publish(state: &State, channel: Id, connection: Id) -> bool {
    state.connection == ConnectionState::Connected
        && state
            .voice_states
            .get(&connection)
            .is_some_and(|v| v.channel_id.as_deref() == Some(channel.as_bytes().as_slice()))
        && holds_in(
            state,
            channel,
            Permissions::VIEW_CHANNEL | Permissions::STREAM,
        )
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

    #[test]
    fn only_administrators_act_on_administrators() {
        // T-090: a non-administrator never acts on an administrator, even
        // from a role placed above the administrator's.
        let (own, admin) = (Id::generate(), Id::generate());
        let mut state = state(own);
        grant(&mut state, own, Permissions::KICK_MEMBERS, 9);
        grant(&mut state, admin, Permissions::ADMINISTRATOR, 1);
        let authority = Authority::own(&state).unwrap();
        assert!(!authority.outranks(&state, admin));
        assert_eq!(
            authority.rights_over(&state, admin),
            ModerationRights::default()
        );
    }

    fn role(state: &State, position: i32) -> &v1::Role {
        state
            .roles
            .values()
            .find(|r| r.position == position)
            .unwrap()
    }

    #[test]
    fn roles_below_the_highest_and_without_administrator_are_editable() {
        let own = Id::generate();
        let mut state = state(own);
        grant(&mut state, own, Permissions::MANAGE_ROLES, 10);
        let holder = Id::generate();
        grant(&mut state, holder, Permissions::SEND_MESSAGES, 1);
        grant(&mut state, holder, Permissions::ADMINISTRATOR, 2);
        grant(&mut state, holder, Permissions::empty(), 11);
        let authority = Authority::own(&state).unwrap();
        assert!(!authority.is_administrator());
        // @everyone and a plain lower role.
        assert!(authority.can_edit_role(role(&state, 0)));
        assert!(authority.can_edit_role(role(&state, 1)));
        // A lower role that carries ADMINISTRATOR.
        assert!(!authority.can_edit_role(role(&state, 2)));
        // The user's own top role, and one above it.
        assert!(!authority.can_edit_role(role(&state, 10)));
        assert!(!authority.can_edit_role(role(&state, 11)));
        assert_eq!(authority.new_role_position(), Some(9));
    }

    #[test]
    fn administrators_edit_every_role_and_create_at_the_bottom() {
        let own = Id::generate();
        let mut state = state(own);
        grant(&mut state, own, Permissions::ADMINISTRATOR, 1);
        grant(&mut state, Id::generate(), Permissions::ADMINISTRATOR, 7);
        let authority = Authority::own(&state).unwrap();
        assert!(authority.is_administrator());
        assert!(state.roles.values().all(|r| authority.can_edit_role(r)));
        assert_eq!(authority.new_role_position(), Some(0));
    }

    #[test]
    fn a_role_from_a_newer_server_is_judged_by_its_known_bits() {
        let own = Id::generate();
        let mut state = state(own);
        grant(&mut state, own, Permissions::MANAGE_ROLES, 10);
        let unknown = 1_u64 << 63;
        let id = Id::generate();
        let role = v1::Role {
            id: bytes(id),
            permissions: unknown | Permissions::SEND_MESSAGES.bits(),
            position: 3,
            ..Default::default()
        };
        let authority = Authority::own(&state).unwrap();
        assert!(authority.can_edit_role(&role));
        let admin = v1::Role {
            permissions: unknown | Permissions::ADMINISTRATOR.bits(),
            ..role
        };
        assert!(!authority.can_edit_role(&admin));
    }

    #[test]
    fn no_position_below_the_lowest_possible_role() {
        let own = Id::generate();
        let mut state = state(own);
        state.roles.clear();
        grant(&mut state, own, Permissions::MANAGE_ROLES, i32::MIN);
        let authority = Authority::own(&state).unwrap();
        assert_eq!(authority.new_role_position(), None);
    }

    #[test]
    fn member_roles_need_manage_roles_and_a_higher_top_role() {
        let (own, peer, junior, senior, admin) = (
            Id::generate(),
            Id::generate(),
            Id::generate(),
            Id::generate(),
            Id::generate(),
        );
        let mut state = state(own);
        grant(&mut state, own, Permissions::MANAGE_ROLES, 10);
        grant(&mut state, peer, Permissions::empty(), 10);
        grant(&mut state, junior, Permissions::empty(), 1);
        grant(&mut state, senior, Permissions::empty(), 11);
        grant(&mut state, admin, Permissions::ADMINISTRATOR, 2);
        let authority = Authority::own(&state).unwrap();
        assert!(authority.can_set_roles_of(&state, junior));
        // A member with only @everyone.
        assert!(authority.can_set_roles_of(&state, Id::generate()));
        assert!(!authority.can_set_roles_of(&state, peer));
        assert!(!authority.can_set_roles_of(&state, senior));
        assert!(!authority.can_set_roles_of(&state, own));
        // T-090: never an administrator, whatever the positions. The shell
        // used to offer this.
        assert!(!authority.can_set_roles_of(&state, admin));

        let mut state = self::state(own);
        grant(&mut state, own, Permissions::MANAGE_CHANNELS, 10);
        grant(&mut state, junior, Permissions::empty(), 1);
        let authority = Authority::own(&state).unwrap();
        assert!(!authority.can_set_roles_of(&state, junior));
    }

    #[test]
    fn administrators_set_anyones_roles_their_own_included() {
        let (own, other_admin, senior) = (Id::generate(), Id::generate(), Id::generate());
        let mut state = state(own);
        grant(&mut state, own, Permissions::ADMINISTRATOR, 1);
        grant(&mut state, other_admin, Permissions::ADMINISTRATOR, 1);
        grant(&mut state, senior, Permissions::empty(), 9);
        let authority = Authority::own(&state).unwrap();
        assert!(authority.allows(Permissions::MANAGE_ROLES));
        for member in [own, other_admin, senior] {
            assert!(authority.can_set_roles_of(&state, member));
        }
    }

    fn channel(state: &mut State, permissions: Option<Permissions>) -> Id {
        let id = Id::generate();
        state.channels.insert(
            id,
            v1::Channel {
                id: bytes(id),
                effective_permissions: permissions.map(|p| p.bits()),
                ..Default::default()
            },
        );
        id
    }

    #[test]
    fn channel_checks_use_the_servers_resolved_mask() {
        let mut state = State::default();
        let all = Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES;
        let open = channel(&mut state, Some(all));
        let unresolved = channel(&mut state, None);
        assert!(holds_in(&state, open, Permissions::VIEW_CHANNEL));
        assert!(holds_in(&state, open, all));
        assert!(!holds_in(
            &state,
            open,
            Permissions::VIEW_CHANNEL | Permissions::ATTACH_FILES
        ));
        assert!(!holds_in(&state, unresolved, Permissions::empty()));
        assert!(!holds_in(&state, Id::generate(), Permissions::empty()));
        let direct = Id::generate();
        state.direct_messages.insert(
            direct,
            v1::Channel {
                effective_permissions: Some(Permissions::DIRECT_MESSAGE.bits()),
                ..Default::default()
            },
        );
        assert!(holds_in(&state, direct, Permissions::SEND_MESSAGES));
        // Bits this build does not know grant nothing, and do not hide the
        // ones it does.
        state.channels.get_mut(&open).unwrap().effective_permissions = Some(all.bits() | 1 << 63);
        assert!(holds_in(&state, open, all));
    }

    #[test]
    fn channel_management_needs_manage_channels_server_wide() {
        let own = Id::generate();
        let mut state = state(own);
        grant(&mut state, own, Permissions::MANAGE_ROLES, 3);
        assert!(!Authority::own(&state).unwrap().can_manage_channels());
        grant(&mut state, own, Permissions::MANAGE_CHANNELS, 4);
        assert!(Authority::own(&state).unwrap().can_manage_channels());
        let mut state = self::state(own);
        grant(&mut state, own, Permissions::ADMINISTRATOR, 1);
        assert!(Authority::own(&state).unwrap().can_manage_channels());
    }

    #[test]
    fn chat_rights_follow_the_channel_mask() {
        let mut state = State::default();
        let hidden = channel(&mut state, Some(Permissions::SEND_MESSAGES));
        assert_eq!(ChatRights::of(&state, hidden), None);
        assert_eq!(ChatRights::of(&state, Id::generate()), None);
        let read_only = channel(&mut state, Some(Permissions::VIEW_CHANNEL));
        assert_eq!(
            ChatRights::of(&state, read_only),
            Some(ChatRights::default())
        );
        let full = channel(
            &mut state,
            Some(
                Permissions::VIEW_CHANNEL
                    | Permissions::SEND_MESSAGES
                    | Permissions::ATTACH_FILES
                    | Permissions::MANAGE_MESSAGES,
            ),
        );
        assert_eq!(
            ChatRights::of(&state, full),
            Some(ChatRights {
                send: true,
                attach: true,
                manage: true,
            })
        );
        let moderator = channel(
            &mut state,
            Some(Permissions::VIEW_CHANNEL | Permissions::MANAGE_MESSAGES),
        );
        assert_eq!(
            ChatRights::of(&state, moderator),
            Some(ChatRights {
                manage: true,
                ..Default::default()
            })
        );
    }

    #[test]
    fn attaching_also_needs_send_messages() {
        // T-090: the server checks SEND_MESSAGES before ATTACH_FILES, so a
        // file alone cannot be sent. The shell offered it on ATTACH_FILES.
        let mut state = State::default();
        let channel = channel(
            &mut state,
            Some(Permissions::VIEW_CHANNEL | Permissions::ATTACH_FILES),
        );
        assert_eq!(ChatRights::of(&state, channel), Some(ChatRights::default()));
    }

    fn seated(state: &mut State, connection: Id, channel: Id) {
        state.voice_states.insert(
            connection,
            v1::VoiceState {
                channel_id: Some(bytes(channel)),
                ..Default::default()
            },
        );
    }

    #[test]
    fn screen_share_needs_a_live_seat_in_the_channel_and_stream() {
        let mut state = State {
            connection: ConnectionState::Connected,
            ..Default::default()
        };
        let full = Permissions::STREAM | Permissions::CONNECT | Permissions::VIEW_CHANNEL;
        let room = channel(&mut state, Some(full));
        let elsewhere = channel(&mut state, Some(full));
        let connection = Id::generate();
        seated(&mut state, connection, room);
        assert!(can_publish(&state, room, connection));
        assert!(!can_publish(&state, room, Id::generate()));
        assert!(!can_publish(&state, elsewhere, connection));
        state.voice_states.get_mut(&connection).unwrap().channel_id = None;
        assert!(!can_publish(&state, room, connection));
        seated(&mut state, connection, room);
        for missing in [Permissions::STREAM, Permissions::VIEW_CHANNEL] {
            state.channels.get_mut(&room).unwrap().effective_permissions =
                Some(full.difference(missing).bits());
            assert!(!can_publish(&state, room, connection), "{missing:?}");
        }
        state.channels.get_mut(&room).unwrap().effective_permissions = Some(full.bits());
        state.connection = ConnectionState::Disconnected;
        assert!(!can_publish(&state, room, connection));
    }

    #[test]
    fn a_moved_member_without_connect_may_still_share() {
        // T-090: a seat placed by a moderator move is kept without CONNECT,
        // and the server's StartStream checks only VIEW_CHANNEL and STREAM.
        // The shell also required CONNECT.
        let mut state = State {
            connection: ConnectionState::Connected,
            ..Default::default()
        };
        let room = channel(
            &mut state,
            Some(Permissions::STREAM | Permissions::VIEW_CHANNEL),
        );
        let connection = Id::generate();
        seated(&mut state, connection, room);
        assert!(can_publish(&state, room, connection));
    }
}
