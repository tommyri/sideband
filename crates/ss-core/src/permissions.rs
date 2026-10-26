//! Permission bits and the channel resolution rules from the data model.

use crate::channel::ChannelKind;

bitflags::bitflags! {
    /// Permission bits. Positions match `docs/plan/03-data-model.md`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Permissions: u64 {
        /// Everything. Bypasses channel overrides.
        ///
        /// Only owner-created roles can carry this bit.
        const ADMINISTRATOR = 1_u64 << 0;
        /// Server settings, invites, and webhooks.
        const MANAGE_SERVER = 1_u64 << 1;
        /// Create, update, delete, and move channels, and edit their overrides.
        const MANAGE_CHANNELS = 1_u64 << 2;
        /// Create, update, and delete roles below the actor's highest role, and assign them.
        const MANAGE_ROLES = 1_u64 << 3;
        /// Delete other people's messages and pin messages.
        const MANAGE_MESSAGES = 1_u64 << 4;
        /// Kick members from the server.
        const KICK_MEMBERS = 1_u64 << 5;
        /// Ban members from the server.
        const BAN_MEMBERS = 1_u64 << 6;
        /// Server mute and unmute.
        const MUTE_MEMBERS = 1_u64 << 7;
        /// Move members between voice channels.
        const MOVE_MEMBERS = 1_u64 << 8;
        /// See a channel and read its history.
        const VIEW_CHANNEL = 1_u64 << 9;
        /// Send messages.
        const SEND_MESSAGES = 1_u64 << 10;
        /// Attach files to messages.
        const ATTACH_FILES = 1_u64 << 11;
        /// Mention `@everyone`.
        const MENTION_EVERYONE = 1_u64 << 12;
        /// Join a voice channel.
        const CONNECT = 1_u64 << 13;
        /// Send audio in a voice channel.
        const SPEAK = 1_u64 << 14;
        /// Share a screen.
        const STREAM = 1_u64 << 15;
        /// Create invite codes.
        const CREATE_INVITES = 1_u64 << 16;
        /// Read the audit log.
        const VIEW_AUDIT_LOG = 1_u64 << 17;
        /// Change the member's own nickname.
        const CHANGE_NICKNAME = 1_u64 << 18;
    }
}

impl Permissions {
    /// Seeded `@everyone` role.
    ///
    /// `VIEW_CHANNEL`, `SEND_MESSAGES`, `ATTACH_FILES`, `CONNECT`, `SPEAK`,
    /// `STREAM`, `CREATE_INVITES`, and `CHANGE_NICKNAME`.
    pub const EVERYONE: Self = Self::VIEW_CHANNEL
        .union(Self::SEND_MESSAGES)
        .union(Self::ATTACH_FILES)
        .union(Self::CONNECT)
        .union(Self::SPEAK)
        .union(Self::STREAM)
        .union(Self::CREATE_INVITES)
        .union(Self::CHANGE_NICKNAME);

    /// Seeded `Admin` role. The first user (the owner) receives this role.
    pub const ADMIN: Self = Self::ADMINISTRATOR;

    /// What a member of a direct or group DM may do.
    ///
    /// `VIEW_CHANNEL`, `SEND_MESSAGES`, and `ATTACH_FILES`. Non-members get
    /// [`Self::empty`].
    pub const DIRECT_MESSAGE: Self = Self::VIEW_CHANNEL
        .union(Self::SEND_MESSAGES)
        .union(Self::ATTACH_FILES);

    /// Resolves the permissions a user has in a channel.
    ///
    /// `role_permissions` is every role the user holds, including `@everyone`.
    /// This function does not insert [`Self::EVERYONE`] on its own. `overrides`
    /// are the channel overwrites for those roles, in the same order. Roles
    /// past the end of `overrides` are not overwritten. Overrides past the
    /// last role are ignored.
    ///
    /// The base set is the bitwise OR of the role permissions. When that set
    /// contains [`Self::ADMINISTRATOR`], the result is [`Self::all`] and
    /// overrides are skipped. Otherwise each override is applied as
    /// `(base & !deny) | allow`: a bit listed in both deny and allow stays
    /// set, and a later override wins over an earlier one.
    ///
    /// [`ChannelKind::is_direct`] channels ignore roles and overrides. A member
    /// receives [`Self::DIRECT_MESSAGE`]; anyone else receives [`Self::empty`].
    /// `is_member` is only read for those channels.
    #[must_use]
    pub fn resolve(
        role_permissions: impl IntoIterator<Item = Self>,
        overrides: impl IntoIterator<Item = PermissionOverride>,
        kind: ChannelKind,
        is_member: bool,
    ) -> Self {
        if kind.is_direct() {
            return if is_member {
                Self::DIRECT_MESSAGE
            } else {
                Self::empty()
            };
        }

        let mut base = Self::empty();
        let mut role_count = 0_usize;
        for role in role_permissions {
            base = base.union(role);
            role_count = role_count.saturating_add(1);
        }
        if base.contains(Self::ADMINISTRATOR) {
            return Self::all();
        }
        for overwrite in overrides.into_iter().take(role_count) {
            base = overwrite.apply(base);
        }
        base
    }

    /// Whether `actor_pos` strictly outranks `target_pos`.
    ///
    /// Both values are `roles.position` (higher is more powerful). Equal
    /// positions do not pass. The server applies the owner short-circuit
    /// before calling this.
    #[must_use]
    pub const fn hierarchy_allows(actor_pos: i64, target_pos: i64) -> bool {
        actor_pos > target_pos
    }
}

/// A channel overwrite for one role.
///
/// Applying it removes [`Self::deny`], then adds [`Self::allow`]. A bit set
/// in both stays set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct PermissionOverride {
    /// Bits added after the deny mask.
    pub allow: Permissions,
    /// Bits removed before the allow mask.
    pub deny: Permissions,
}

impl PermissionOverride {
    /// An overwrite with these allow and deny masks.
    #[must_use]
    pub const fn new(allow: Permissions, deny: Permissions) -> Self {
        Self { allow, deny }
    }

    /// Applies `(base & !deny) | allow`.
    #[must_use]
    pub const fn apply(self, base: Permissions) -> Permissions {
        base.difference(self.deny).union(self.allow)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        channel::ChannelKind,
        permissions::{PermissionOverride, Permissions},
    };

    struct Case {
        name: &'static str,
        roles: Vec<Permissions>,
        overrides: Vec<PermissionOverride>,
        kind: ChannelKind,
        is_member: bool,
        expected: Permissions,
    }

    fn ov(allow: Permissions, deny: Permissions) -> PermissionOverride {
        PermissionOverride::new(allow, deny)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the body is the resolution table the acceptance test covers"
    )]
    fn cases() -> Vec<Case> {
        let none = Permissions::empty();
        let view = Permissions::VIEW_CHANNEL;
        let send = Permissions::SEND_MESSAGES;
        let attach = Permissions::ATTACH_FILES;
        vec![
            Case {
                name: "administrator bypasses overrides",
                roles: vec![Permissions::ADMIN.union(view), Permissions::EVERYONE],
                overrides: vec![ov(none, Permissions::all()), ov(none, Permissions::all())],
                kind: ChannelKind::Text,
                is_member: false,
                expected: Permissions::all(),
            },
            Case {
                name: "administrator with no overrides is everything",
                roles: vec![Permissions::ADMINISTRATOR],
                overrides: vec![],
                kind: ChannelKind::Voice,
                is_member: false,
                expected: Permissions::all(),
            },
            Case {
                name: "allow wins over deny in one override",
                roles: vec![view.union(send)],
                overrides: vec![ov(view, view.union(send))],
                kind: ChannelKind::Text,
                is_member: true,
                expected: view,
            },
            Case {
                name: "a later role can allow a bit an earlier role denied",
                roles: vec![view.union(send), none],
                overrides: vec![ov(none, send), ov(send, none)],
                kind: ChannelKind::Text,
                is_member: false,
                expected: view.union(send),
            },
            Case {
                name: "a later role can deny a bit an earlier role allowed",
                roles: vec![send, none],
                overrides: vec![ov(none, none), ov(none, send)],
                kind: ChannelKind::Category,
                is_member: false,
                expected: none,
            },
            Case {
                name: "multiple roles are ored with everyone",
                roles: vec![
                    Permissions::EVERYONE,
                    Permissions::KICK_MEMBERS,
                    Permissions::BAN_MEMBERS,
                ],
                overrides: vec![],
                kind: ChannelKind::Voice,
                is_member: false,
                expected: Permissions::EVERYONE
                    .union(Permissions::KICK_MEMBERS)
                    .union(Permissions::BAN_MEMBERS),
            },
            Case {
                name: "multiple role overrides stack",
                roles: vec![
                    Permissions::EVERYONE,
                    Permissions::KICK_MEMBERS,
                    Permissions::BAN_MEMBERS,
                ],
                overrides: vec![
                    ov(none, Permissions::SPEAK),
                    ov(Permissions::MUTE_MEMBERS, Permissions::KICK_MEMBERS),
                    ov(none, Permissions::BAN_MEMBERS),
                ],
                kind: ChannelKind::Text,
                is_member: false,
                expected: Permissions::VIEW_CHANNEL
                    .union(send)
                    .union(attach)
                    .union(Permissions::CONNECT)
                    .union(Permissions::STREAM)
                    .union(Permissions::CREATE_INVITES)
                    .union(Permissions::CHANGE_NICKNAME)
                    .union(Permissions::MUTE_MEMBERS),
            },
            Case {
                name: "roles without a paired override keep their base bits",
                roles: vec![view, send],
                overrides: vec![ov(none, view)],
                kind: ChannelKind::Text,
                is_member: false,
                expected: send,
            },
            Case {
                name: "overrides past the last role are ignored",
                roles: vec![view],
                overrides: vec![ov(none, none), ov(none, view)],
                kind: ChannelKind::Text,
                is_member: false,
                expected: view,
            },
            Case {
                name: "dm member ignores roles and overrides",
                roles: vec![Permissions::ADMINISTRATOR, Permissions::EVERYONE],
                overrides: vec![ov(none, Permissions::all()), ov(none, Permissions::all())],
                kind: ChannelKind::Dm,
                is_member: true,
                expected: view.union(send).union(attach),
            },
            Case {
                name: "group dm member gets the direct-message set",
                roles: vec![],
                overrides: vec![],
                kind: ChannelKind::GroupDm,
                is_member: true,
                expected: view.union(send).union(attach),
            },
            Case {
                name: "dm non-member gets nothing",
                roles: vec![Permissions::EVERYONE],
                overrides: vec![],
                kind: ChannelKind::Dm,
                is_member: false,
                expected: none,
            },
            Case {
                name: "group dm non-member gets nothing even with administrator",
                roles: vec![Permissions::ADMINISTRATOR],
                overrides: vec![],
                kind: ChannelKind::GroupDm,
                is_member: false,
                expected: none,
            },
            Case {
                name: "guild channels ignore the membership flag",
                roles: vec![Permissions::EVERYONE],
                overrides: vec![],
                kind: ChannelKind::Category,
                is_member: false,
                expected: Permissions::EVERYONE,
            },
            Case {
                name: "no roles on a guild channel is empty",
                roles: vec![],
                overrides: vec![ov(view, none)],
                kind: ChannelKind::Text,
                is_member: true,
                expected: none,
            },
        ]
    }

    #[test]
    fn resolve_matches_the_permission_table() {
        for case in cases() {
            let got = Permissions::resolve(
                case.roles.iter().copied(),
                case.overrides.iter().copied(),
                case.kind,
                case.is_member,
            );
            let name = case.name;
            assert_eq!(got, case.expected, "case {name}");
        }
    }

    #[test]
    fn flag_bits_match_the_data_model() {
        let flags = [
            (Permissions::ADMINISTRATOR, 0),
            (Permissions::MANAGE_SERVER, 1),
            (Permissions::MANAGE_CHANNELS, 2),
            (Permissions::MANAGE_ROLES, 3),
            (Permissions::MANAGE_MESSAGES, 4),
            (Permissions::KICK_MEMBERS, 5),
            (Permissions::BAN_MEMBERS, 6),
            (Permissions::MUTE_MEMBERS, 7),
            (Permissions::MOVE_MEMBERS, 8),
            (Permissions::VIEW_CHANNEL, 9),
            (Permissions::SEND_MESSAGES, 10),
            (Permissions::ATTACH_FILES, 11),
            (Permissions::MENTION_EVERYONE, 12),
            (Permissions::CONNECT, 13),
            (Permissions::SPEAK, 14),
            (Permissions::STREAM, 15),
            (Permissions::CREATE_INVITES, 16),
            (Permissions::VIEW_AUDIT_LOG, 17),
            (Permissions::CHANGE_NICKNAME, 18),
        ];
        for (flag, bit) in flags {
            assert_eq!(flag.bits(), 1_u64 << bit);
        }
        let defined = (1_u64 << 19) - 1;
        assert_eq!(Permissions::all().bits(), defined);
        assert_eq!(Permissions::from_bits(defined), Some(Permissions::all()));
        assert_eq!(Permissions::from_bits(1_u64 << 19), None);
    }

    #[test]
    fn seeded_roles_match_the_data_model() {
        assert_eq!(
            Permissions::EVERYONE,
            Permissions::VIEW_CHANNEL
                .union(Permissions::SEND_MESSAGES)
                .union(Permissions::ATTACH_FILES)
                .union(Permissions::CONNECT)
                .union(Permissions::SPEAK)
                .union(Permissions::STREAM)
                .union(Permissions::CREATE_INVITES)
                .union(Permissions::CHANGE_NICKNAME)
        );
        assert_eq!(Permissions::ADMIN, Permissions::ADMINISTRATOR);
        assert!(!Permissions::EVERYONE.contains(Permissions::ADMINISTRATOR));
        assert_eq!(
            Permissions::DIRECT_MESSAGE,
            Permissions::VIEW_CHANNEL
                .union(Permissions::SEND_MESSAGES)
                .union(Permissions::ATTACH_FILES)
        );
    }

    #[test]
    fn hierarchy_is_strictly_greater_position() {
        assert!(Permissions::hierarchy_allows(2, 1));
        assert!(!Permissions::hierarchy_allows(1, 2));
        assert!(!Permissions::hierarchy_allows(0, 0));
        assert!(Permissions::hierarchy_allows(0, -1));
        assert!(!Permissions::hierarchy_allows(i64::MIN, i64::MIN));
        assert!(Permissions::hierarchy_allows(i64::MAX, i64::MAX - 1));
    }
}
