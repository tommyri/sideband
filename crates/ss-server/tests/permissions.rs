//! Role hierarchy, channel overrides, and the permission snapshot.
//!
//! Each command case opens its own database so an allow cannot leak into the
//! next row. Checks share one world because they do not write.

use ss_core::{ChannelKind, Id, Permissions, Timestamp, UserKind};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::id_to_bytes,
};
use ss_server::{
    db::{
        self, ADMIN_ROLE_ID, EVERYONE_ROLE_ID, NewChannel, NewRole, NewUser, audit_log, channels,
        roles, users,
    },
    handlers::{self, Roles},
    perms::{self, PermissionService},
};
use std::sync::Arc;

const ADMIN_BIT: u64 = Permissions::ADMINISTRATOR.bits();
const SEND_BIT: u64 = Permissions::SEND_MESSAGES.bits();
const UNKNOWN_BIT: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who {
    Admin,
    Manager,
    Member,
    Senior,
}

#[derive(Clone, Copy, Debug)]
enum RoleRef {
    Everyone,
    Admin,
    Helper,
    Manager,
    Senior,
    Missing,
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Server,
    Text,
    Hidden,
    Dm,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Allowed,
    Denied,
    Missing,
    Invalid,
}

#[derive(Clone, Copy)]
enum Action {
    Create {
        position: i32,
        permissions: u64,
    },
    Update {
        role: RoleRef,
        name: Option<&'static str>,
        permissions: Option<u64>,
        position: Option<i32>,
    },
    Delete {
        role: RoleRef,
    },
    SetRoles {
        target: Who,
        roles: &'static [RoleRef],
    },
    Override {
        channel: Scope,
        role: RoleRef,
        deny: u64,
    },
}

struct CommandCase {
    name: &'static str,
    actor: Who,
    action: Action,
    expect: Outcome,
}

struct CheckCase {
    name: &'static str,
    actor: Who,
    scope: Scope,
    permission: Permissions,
    expect: Outcome,
}

const HELPER_ONLY: &[RoleRef] = &[RoleRef::Helper];
const NO_ROLES: &[RoleRef] = &[];

const COMMANDS: &[CommandCase] = &[
    CommandCase {
        name: "member cannot create a role",
        actor: Who::Member,
        action: Action::Create {
            position: 1,
            permissions: 0,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "a higher role without manage-roles cannot create",
        actor: Who::Senior,
        action: Action::Create {
            position: 1,
            permissions: 0,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager creates a role below their own",
        actor: Who::Manager,
        action: Action::Create {
            position: 2,
            permissions: 0,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "manager cannot create a role at their own position",
        actor: Who::Manager,
        action: Action::Create {
            position: 3,
            permissions: 0,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager cannot grant administrator",
        actor: Who::Manager,
        action: Action::Create {
            position: 2,
            permissions: ADMIN_BIT,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "administrator bypasses hierarchy and can grant administrator",
        actor: Who::Admin,
        action: Action::Create {
            position: 50,
            permissions: ADMIN_BIT,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "unknown permission bits are rejected",
        actor: Who::Manager,
        action: Action::Create {
            position: 2,
            permissions: UNKNOWN_BIT,
        },
        expect: Outcome::Invalid,
    },
    CommandCase {
        name: "manager cannot raise a role to their own position",
        actor: Who::Manager,
        action: Action::Update {
            role: RoleRef::Helper,
            name: None,
            permissions: None,
            position: Some(3),
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "an empty role name is invalid",
        actor: Who::Manager,
        action: Action::Update {
            role: RoleRef::Helper,
            name: Some(""),
            permissions: None,
            position: None,
        },
        expect: Outcome::Invalid,
    },
    CommandCase {
        name: "administrator cannot delete @everyone",
        actor: Who::Admin,
        action: Action::Delete {
            role: RoleRef::Everyone,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager cannot delete @everyone",
        actor: Who::Manager,
        action: Action::Delete {
            role: RoleRef::Everyone,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "member cannot delete a role",
        actor: Who::Member,
        action: Action::Delete {
            role: RoleRef::Helper,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager deletes a role below them",
        actor: Who::Manager,
        action: Action::Delete {
            role: RoleRef::Helper,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "a missing role is not found",
        actor: Who::Manager,
        action: Action::Delete {
            role: RoleRef::Missing,
        },
        expect: Outcome::Missing,
    },
    CommandCase {
        name: "member cannot assign roles",
        actor: Who::Member,
        action: Action::SetRoles {
            target: Who::Member,
            roles: HELPER_ONLY,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager grants a lower role to a lower member",
        actor: Who::Manager,
        action: Action::SetRoles {
            target: Who::Member,
            roles: HELPER_ONLY,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "manager cannot change a member who outranks them",
        actor: Who::Manager,
        action: Action::SetRoles {
            target: Who::Senior,
            roles: HELPER_ONLY,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager cannot strip the administrator role",
        actor: Who::Manager,
        action: Action::SetRoles {
            target: Who::Admin,
            roles: NO_ROLES,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "administrator can change a higher member",
        actor: Who::Admin,
        action: Action::SetRoles {
            target: Who::Senior,
            roles: HELPER_ONLY,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "member cannot set a channel override",
        actor: Who::Member,
        action: Action::Override {
            channel: Scope::Text,
            role: RoleRef::Helper,
            deny: SEND_BIT,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager sets an override on a lower role",
        actor: Who::Manager,
        action: Action::Override {
            channel: Scope::Text,
            role: RoleRef::Helper,
            deny: SEND_BIT,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "manager cannot override a role at their position",
        actor: Who::Manager,
        action: Action::Override {
            channel: Scope::Text,
            role: RoleRef::Manager,
            deny: SEND_BIT,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "manager cannot override the administrator role",
        actor: Who::Manager,
        action: Action::Override {
            channel: Scope::Text,
            role: RoleRef::Admin,
            deny: SEND_BIT,
        },
        expect: Outcome::Denied,
    },
    CommandCase {
        name: "administrator overrides a higher role",
        actor: Who::Admin,
        action: Action::Override {
            channel: Scope::Text,
            role: RoleRef::Senior,
            deny: SEND_BIT,
        },
        expect: Outcome::Allowed,
    },
    CommandCase {
        name: "a missing channel is not found",
        actor: Who::Manager,
        action: Action::Override {
            channel: Scope::Missing,
            role: RoleRef::Everyone,
            deny: SEND_BIT,
        },
        expect: Outcome::Missing,
    },
    CommandCase {
        name: "unknown override bits are rejected",
        actor: Who::Manager,
        action: Action::Override {
            channel: Scope::Text,
            role: RoleRef::Helper,
            deny: UNKNOWN_BIT,
        },
        expect: Outcome::Invalid,
    },
];

const CHECKS: &[CheckCase] = &[
    CheckCase {
        name: "member can view a normal channel",
        actor: Who::Member,
        scope: Scope::Text,
        permission: Permissions::VIEW_CHANNEL,
        expect: Outcome::Allowed,
    },
    CheckCase {
        name: "everyone deny hides a channel",
        actor: Who::Member,
        scope: Scope::Hidden,
        permission: Permissions::VIEW_CHANNEL,
        expect: Outcome::Denied,
    },
    CheckCase {
        name: "administrator bypasses a deny override",
        actor: Who::Admin,
        scope: Scope::Hidden,
        permission: Permissions::VIEW_CHANNEL,
        expect: Outcome::Allowed,
    },
    CheckCase {
        name: "a direct-message member can send",
        actor: Who::Member,
        scope: Scope::Dm,
        permission: Permissions::SEND_MESSAGES,
        expect: Outcome::Allowed,
    },
    CheckCase {
        name: "a direct-message outsider cannot send",
        actor: Who::Manager,
        scope: Scope::Dm,
        permission: Permissions::SEND_MESSAGES,
        expect: Outcome::Denied,
    },
    CheckCase {
        name: "administrator is still an outsider in a direct message",
        actor: Who::Admin,
        scope: Scope::Dm,
        permission: Permissions::VIEW_CHANNEL,
        expect: Outcome::Denied,
    },
    CheckCase {
        name: "member lacks manage-roles",
        actor: Who::Member,
        scope: Scope::Server,
        permission: Permissions::MANAGE_ROLES,
        expect: Outcome::Denied,
    },
    CheckCase {
        name: "manager holds manage-roles and manage-channels",
        actor: Who::Manager,
        scope: Scope::Server,
        permission: Permissions::MANAGE_CHANNELS,
        expect: Outcome::Allowed,
    },
    CheckCase {
        name: "a missing channel is not found",
        actor: Who::Member,
        scope: Scope::Missing,
        permission: Permissions::VIEW_CHANNEL,
        expect: Outcome::Missing,
    },
];

const VISIBLE: &[(Who, Scope, bool)] = &[
    (Who::Member, Scope::Text, true),
    (Who::Member, Scope::Hidden, false),
    (Who::Member, Scope::Dm, true),
    (Who::Manager, Scope::Dm, false),
    (Who::Admin, Scope::Hidden, true),
    (Who::Admin, Scope::Dm, false),
];

const ACTS: &[(Who, Who, bool)] = &[
    (Who::Senior, Who::Member, true),
    (Who::Manager, Who::Senior, false),
    (Who::Admin, Who::Senior, true),
    (Who::Manager, Who::Manager, false),
    (Who::Senior, Who::Admin, true),
];

struct World {
    pool: Arc<db::Pool>,
    perms: Arc<PermissionService>,
    roles_handler: Roles,
    admin: Id,
    manager: Id,
    member: Id,
    senior: Id,
    helper: Id,
    manager_role: Id,
    senior_role: Id,
    text: Id,
    hidden: Id,
    dm: Id,
    deleted: Id,
    _dir: tempfile::TempDir,
}

impl World {
    fn user(&self, who: Who) -> Id {
        match who {
            Who::Admin => self.admin,
            Who::Manager => self.manager,
            Who::Member => self.member,
            Who::Senior => self.senior,
        }
    }

    fn role(&self, role: RoleRef) -> Id {
        match role {
            RoleRef::Everyone => EVERYONE_ROLE_ID,
            RoleRef::Admin => ADMIN_ROLE_ID,
            RoleRef::Helper => self.helper,
            RoleRef::Manager => self.manager_role,
            RoleRef::Senior => self.senior_role,
            RoleRef::Missing => Id::generate(),
        }
    }

    fn channel(&self, scope: Scope) -> Option<Id> {
        match scope {
            Scope::Server => None,
            Scope::Text => Some(self.text),
            Scope::Hidden => Some(self.hidden),
            Scope::Dm => Some(self.dm),
            Scope::Missing => Some(Id::generate()),
        }
    }
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let ids = {
        let mut conn = pool.write().await;
        let admin = insert_user(&mut conn, "admin").await;
        let manager = insert_user(&mut conn, "manager").await;
        let member = insert_user(&mut conn, "member").await;
        let senior = insert_user(&mut conn, "senior").await;
        let manage = Permissions::MANAGE_ROLES.union(Permissions::MANAGE_CHANNELS);
        let helper = insert_role(&mut conn, "helper", Permissions::empty(), 2).await;
        let manager_role = insert_role(&mut conn, "manager", manage, 3).await;
        let senior_role = insert_role(&mut conn, "senior", Permissions::empty(), 4).await;
        roles::assign(&mut conn, admin, ADMIN_ROLE_ID)
            .await
            .unwrap();
        roles::assign(&mut conn, manager, manager_role)
            .await
            .unwrap();
        roles::assign(&mut conn, senior, senior_role).await.unwrap();
        let text = insert_channel(&mut conn, ChannelKind::Text, "general", admin).await;
        let hidden = insert_channel(&mut conn, ChannelKind::Text, "hidden", admin).await;
        let dm = insert_channel(&mut conn, ChannelKind::Dm, "dm", admin).await;
        let deleted = insert_channel(&mut conn, ChannelKind::Text, "gone", admin).await;
        channels::set_override(
            &mut conn,
            hidden,
            EVERYONE_ROLE_ID,
            Permissions::empty(),
            Permissions::VIEW_CHANNEL,
        )
        .await
        .unwrap();
        channels::set_override(
            &mut conn,
            dm,
            EVERYONE_ROLE_ID,
            Permissions::empty(),
            Permissions::SEND_MESSAGES,
        )
        .await
        .unwrap();
        channels::add_member(&mut conn, dm, member).await.unwrap();
        channels::soft_delete(&mut conn, deleted, Timestamp::now())
            .await
            .unwrap();
        (
            admin,
            manager,
            member,
            senior,
            helper,
            manager_role,
            senior_role,
            text,
            hidden,
            dm,
            deleted,
        )
    };
    let perms = Arc::new(PermissionService::open(Arc::clone(&pool)).await.unwrap());
    let roles_handler = Roles::new(Arc::clone(&perms));
    World {
        pool,
        perms,
        roles_handler,
        admin: ids.0,
        manager: ids.1,
        member: ids.2,
        senior: ids.3,
        helper: ids.4,
        manager_role: ids.5,
        senior_role: ids.6,
        text: ids.7,
        hidden: ids.8,
        dm: ids.9,
        deleted: ids.10,
        _dir: dir,
    }
}

async fn insert_user(conn: &mut sqlx::SqliteConnection, username: &str) -> Id {
    let id = Id::generate();
    users::insert(
        conn,
        NewUser {
            id,
            username: username.to_owned(),
            display_name: username.to_owned(),
            password_hash: "hash".to_owned(),
            kind: UserKind::User,
        },
    )
    .await
    .unwrap();
    id
}

async fn insert_role(
    conn: &mut sqlx::SqliteConnection,
    name: &str,
    permissions: Permissions,
    position: i64,
) -> Id {
    let id = Id::generate();
    roles::insert(
        conn,
        NewRole {
            id,
            name: name.to_owned(),
            color: None,
            permissions,
            position,
        },
    )
    .await
    .unwrap();
    id
}

async fn insert_channel(
    conn: &mut sqlx::SqliteConnection,
    kind: ChannelKind,
    name: &str,
    created_by: Id,
) -> Id {
    let id = Id::generate();
    channels::insert(
        conn,
        NewChannel {
            id,
            kind,
            name: name.to_owned(),
            topic: None,
            parent_id: None,
            position: 0,
            voice_profile: None,
            voice_bitrate: None,
            user_limit: None,
            created_by,
        },
    )
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn command_matrix() {
    for case in COMMANDS {
        let world = world().await;
        let result = apply(&world, case.actor, case.action).await;
        assert_eq!(classify(result), case.expect, "{}", case.name);
        let audited = audit_rows(&world).await;
        if case.expect == Outcome::Allowed {
            assert_eq!(audited.len(), 1, "{}", case.name);
            assert_eq!(audited[0].0, action_name(case.action), "{}", case.name);
            assert_eq!(audited[0].1, Some(world.user(case.actor)), "{}", case.name);
        } else {
            assert!(audited.is_empty(), "{}", case.name);
        }
    }
}

#[tokio::test]
async fn permission_matrix_covers_overrides_hierarchy_and_direct_messages() {
    let world = world().await;
    for case in CHECKS {
        let result = world.perms.check(
            world.user(case.actor),
            world.channel(case.scope),
            case.permission,
        );
        assert_eq!(classify_access(&result), case.expect, "{}", case.name);
    }
    for (who, scope, sees) in VISIBLE {
        let visible = world.perms.visible_channels(world.user(*who));
        let channel = world.channel(*scope).unwrap();
        assert_eq!(visible.contains(&channel), *sees, "{who:?} {scope:?}");
    }
    assert!(
        !world
            .perms
            .visible_channels(world.member)
            .contains(&world.deleted)
    );
    for (actor, target, allowed) in ACTS {
        assert_eq!(
            world
                .perms
                .can_act_on(world.user(*actor), world.user(*target)),
            *allowed,
            "{actor:?} on {target:?}"
        );
    }
}

#[tokio::test]
async fn role_edits_show_up_on_the_next_check() {
    let world = world().await;
    let channel = Some(world.text);
    assert!(
        world
            .perms
            .check(world.member, channel, Permissions::SEND_MESSAGES)
            .is_ok()
    );
    let reply = world
        .roles_handler
        .update(
            world.admin,
            &v1::UpdateRole {
                role_id: id_to_bytes(EVERYONE_ROLE_ID),
                name: None,
                color: None,
                permissions: Some(
                    Permissions::EVERYONE
                        .difference(Permissions::SEND_MESSAGES)
                        .bits(),
                ),
                position: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        reply.events.first(),
        Some(&server_envelope::Msg::RoleUpdated(_))
    ));
    assert!(
        world
            .perms
            .check(world.member, channel, Permissions::SEND_MESSAGES)
            .is_err()
    );
    assert!(
        world
            .perms
            .check(world.admin, channel, Permissions::SEND_MESSAGES)
            .is_ok()
    );
}

#[tokio::test]
async fn override_edits_show_up_on_the_next_check() {
    let world = world().await;
    let reply = world
        .roles_handler
        .set_channel_override(
            world.manager,
            &v1::SetChannelOverride {
                channel_id: id_to_bytes(world.text),
                role_id: id_to_bytes(EVERYONE_ROLE_ID),
                allow: 0,
                deny: SEND_BIT,
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(reply.events.as_slice(), [server_envelope::Msg::ChannelUpdated(event)]
        if event.channel.as_ref().is_some_and(|channel| channel.id == id_to_bytes(world.text)))
    );
    assert!(matches!(reply.ack, ack::Result::Empty(_)));
    assert!(
        world
            .perms
            .check(world.member, Some(world.text), Permissions::SEND_MESSAGES)
            .is_err()
    );
    assert!(
        world
            .perms
            .check(world.member, Some(world.text), Permissions::VIEW_CHANNEL)
            .is_ok()
    );
    assert!(
        world
            .perms
            .check(world.admin, Some(world.text), Permissions::SEND_MESSAGES)
            .is_ok()
    );
}

#[tokio::test]
async fn deleting_a_role_compacts_positions_and_clears_grants() {
    let world = world().await;
    world
        .roles_handler
        .set_user_roles(
            world.manager,
            &v1::SetUserRoles {
                user_id: id_to_bytes(world.member),
                role_ids: vec![id_to_bytes(world.helper)],
            },
        )
        .await
        .unwrap();
    let reply = world
        .roles_handler
        .delete(
            world.manager,
            &v1::DeleteRole {
                role_id: id_to_bytes(world.helper),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        reply.events.first(),
        Some(&server_envelope::Msg::RoleDeleted(_))
    ));
    assert!(matches!(reply.ack, ack::Result::Empty(_)));
    let mut conn = world.pool.write().await;
    assert!(roles::get(&mut conn, world.helper).await.unwrap().is_none());
    assert_eq!(
        roles::roles_for_user(&mut conn, world.member)
            .await
            .unwrap(),
        Vec::new()
    );
    assert_eq!(position(&mut conn, EVERYONE_ROLE_ID).await, 0);
    assert_eq!(position(&mut conn, ADMIN_ROLE_ID).await, 1);
    assert_eq!(position(&mut conn, world.manager_role).await, 2);
    assert_eq!(position(&mut conn, world.senior_role).await, 3);
    drop(conn);
    assert_eq!(world.perms.highest_role_position(world.manager), 2);
    assert_eq!(world.perms.highest_role_position(world.senior), 3);
}

#[tokio::test]
async fn create_role_keeps_the_requested_position_and_color() {
    let world = world().await;
    let reply = world
        .roles_handler
        .create(
            world.admin,
            &v1::CreateRole {
                name: "mods".into(),
                color: Some(0x0011_2233),
                permissions: 0,
                position: 2,
            },
        )
        .await
        .unwrap();
    let Some(server_envelope::Msg::RoleCreated(created)) = reply.events.first() else {
        panic!("expected RoleCreated");
    };
    let role = created.role.clone().expect("role");
    assert_eq!(role.name, "mods");
    assert_eq!(role.position, 2);
    assert_eq!(role.color, Some(0x0011_2233));
    assert!(!role.is_default);
    assert!(matches!(reply.ack, ack::Result::Role(_)));
    let mut conn = world.pool.write().await;
    assert_eq!(position(&mut conn, ADMIN_ROLE_ID).await, 1);
    assert_eq!(position(&mut conn, world.manager_role).await, 3);
}

async fn apply(
    world: &World,
    actor: Who,
    action: Action,
) -> Result<handlers::CommandReply, handlers::Error> {
    let actor = world.user(actor);
    match action {
        Action::Create {
            position,
            permissions,
        } => {
            world
                .roles_handler
                .create(
                    actor,
                    &v1::CreateRole {
                        name: "created".into(),
                        color: None,
                        permissions,
                        position,
                    },
                )
                .await
        }
        Action::Update {
            role,
            name,
            permissions,
            position,
        } => {
            world
                .roles_handler
                .update(
                    actor,
                    &v1::UpdateRole {
                        role_id: id_to_bytes(world.role(role)),
                        name: name.map(str::to_owned),
                        color: None,
                        permissions,
                        position,
                    },
                )
                .await
        }
        Action::Delete { role } => {
            world
                .roles_handler
                .delete(
                    actor,
                    &v1::DeleteRole {
                        role_id: id_to_bytes(world.role(role)),
                    },
                )
                .await
        }
        Action::SetRoles {
            target,
            roles: granted,
        } => {
            world
                .roles_handler
                .set_user_roles(
                    actor,
                    &v1::SetUserRoles {
                        user_id: id_to_bytes(world.user(target)),
                        role_ids: granted
                            .iter()
                            .map(|role| id_to_bytes(world.role(*role)))
                            .collect(),
                    },
                )
                .await
        }
        Action::Override {
            channel,
            role,
            deny,
        } => {
            let channel_id = world.channel(channel).unwrap_or_else(Id::generate);
            world
                .roles_handler
                .set_channel_override(
                    actor,
                    &v1::SetChannelOverride {
                        channel_id: id_to_bytes(channel_id),
                        role_id: id_to_bytes(world.role(role)),
                        allow: 0,
                        deny,
                    },
                )
                .await
        }
    }
}

fn classify(result: Result<handlers::CommandReply, handlers::Error>) -> Outcome {
    match result {
        Ok(_) => Outcome::Allowed,
        Err(handlers::Error::Forbidden(_)) => Outcome::Denied,
        Err(handlers::Error::NotFound(_)) => Outcome::Missing,
        Err(handlers::Error::Invalid(_)) => Outcome::Invalid,
        Err(error) => panic!("{error}"),
    }
}

fn classify_access(result: &Result<(), perms::AccessError>) -> Outcome {
    match result {
        Ok(()) => Outcome::Allowed,
        Err(perms::AccessError::Forbidden) => Outcome::Denied,
        Err(perms::AccessError::NotFound) => Outcome::Missing,
    }
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::Create { .. } => "role.create",
        Action::Update { .. } => "role.update",
        Action::Delete { .. } => "role.delete",
        Action::SetRoles { .. } => "member.roles.set",
        Action::Override { .. } => "channel.override.set",
    }
}

async fn audit_rows(world: &World) -> Vec<(String, Option<Id>)> {
    let mut conn = world.pool.write().await;
    audit_log::recent(&mut conn, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.action, row.actor_id))
        .collect()
}

async fn position(conn: &mut sqlx::SqliteConnection, id: Id) -> i64 {
    roles::get(conn, id).await.unwrap().unwrap().position
}
