//! Channel tree, default snapshot, and visibility of channel events.

use ss_core::{Id, Permissions, UserKind, VoiceProfile};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};
use ss_server::{
    db::{
        self, ADMIN_ROLE_ID, EVERYONE_ROLE_ID, NewMessage, NewUser, channels, messages, roles,
        users,
    },
    handlers::{self, Channels},
    perms::PermissionService,
    snapshot,
};
use std::sync::Arc;

#[tokio::test]
async fn a_fresh_server_snapshot_has_the_default_tree() {
    let world = world().await;
    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert_eq!(snap.voice_states, Vec::new());
    assert_eq!(snap.tracks, Vec::new());
    assert_eq!(snap.channels.len(), 3);
    let category = find(&snap, "General", v1::ChannelKind::Category);
    let text = find(&snap, "general", v1::ChannelKind::Text);
    let voice = find(&snap, "General", v1::ChannelKind::Voice);
    assert!(category.parent_id.is_none());
    assert_eq!(text.parent_id.as_ref(), Some(&category.id));
    assert_eq!(voice.parent_id.as_ref(), Some(&category.id));
    assert_eq!(text.position, 0);
    assert_eq!(voice.position, 1);
    assert_eq!(
        voice.voice.as_ref().unwrap().profile,
        i32::from(v1::VoiceProfile::Voice)
    );
    assert!(snap.roles.iter().any(|role| role.is_default));
    assert!(
        snap.members
            .iter()
            .any(|member| member.user.as_ref().unwrap().username == "admin")
    );
    let again = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert_eq!(again.channels.len(), 3);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "create, move, and delete are one ordered scenario"
)]
async fn commands_validate_order_and_keep_messages() {
    let world = world().await;
    snapshot::build(&world.perms, world.admin).await.unwrap();
    let denied = world
        .channels
        .create(world.member, &text_channel("nope", None, 0))
        .await;
    assert!(matches!(denied, Err(handlers::Error::Forbidden(_))));

    for (name, command, expect) in invalid_creates() {
        let result = world.channels.create(world.admin, &command).await;
        assert_eq!(class(result), expect, "{name}");
    }

    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    let category = id_of(find(&snap, "General", v1::ChannelKind::Category));
    let text = id_of(find(&snap, "general", v1::ChannelKind::Text));
    let created = world
        .channels
        .create(
            world.admin,
            &text_channel("announcements", Some(category), 0),
        )
        .await
        .unwrap();
    assert!(matches!(created.ack, ack::Result::ChannelId(_)));
    let Some(server_envelope::Msg::ChannelCreated(event)) = created.events.first() else {
        panic!("expected ChannelCreated");
    };
    assert_eq!(event.channel.as_ref().unwrap().position, 0);
    let mut conn = world.pool.write().await;
    assert_eq!(
        channels::get(&mut conn, text)
            .await
            .unwrap()
            .unwrap()
            .position,
        1
    );
    let message_id = Id::generate();
    messages::insert(
        &mut conn,
        NewMessage {
            id: message_id,
            channel_id: text,
            author_id: Some(world.admin),
            webhook_id: None,
            webhook_name: None,
            content: "stay".into(),
            reply_to: None,
            mentions: Vec::new(),
            mentions_everyone: false,
        },
    )
    .await
    .unwrap();
    drop(conn);

    let moved = world
        .channels
        .relocate(
            world.admin,
            &v1::MoveChannel {
                channel_id: id_to_bytes(text),
                parent_id: Some(id_to_bytes(category)),
                position: 2,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        moved.events.first(),
        Some(server_envelope::Msg::ChannelUpdated(_))
    ));
    let mut conn = world.pool.write().await;
    assert_eq!(
        channels::get(&mut conn, text)
            .await
            .unwrap()
            .unwrap()
            .position,
        2
    );
    drop(conn);

    let deleted = world
        .channels
        .delete(
            world.admin,
            &v1::DeleteChannel {
                channel_id: id_to_bytes(category),
            },
        )
        .await
        .unwrap();
    assert!(deleted.events.len() >= 3);
    let mut conn = world.pool.write().await;
    assert!(
        channels::get(&mut conn, category)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert!(
        channels::get(&mut conn, text)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert_eq!(
        messages::get(&mut conn, message_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "stay"
    );
    drop(conn);
    let after = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert!(
        after
            .channels
            .iter()
            .all(|channel| channel.name != "general")
    );
    let mut conn = world.pool.write().await;
    assert!(channels::guild_count(&mut conn).await.unwrap() > 0);
}

#[tokio::test]
async fn a_hidden_channel_is_left_out_of_the_snapshot_and_events() {
    let world = world().await;
    snapshot::build(&world.perms, world.admin).await.unwrap();
    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    let category = id_of(find(&snap, "General", v1::ChannelKind::Category));
    let created = world
        .channels
        .create(world.admin, &text_channel("secret", Some(category), 2))
        .await
        .unwrap();
    let secret = id_of(created.events.first().unwrap().channel_created());
    let mut conn = world.pool.write().await;
    channels::set_override(
        &mut conn,
        secret,
        EVERYONE_ROLE_ID,
        Permissions::empty(),
        Permissions::VIEW_CHANNEL,
    )
    .await
    .unwrap();
    drop(conn);
    world.perms.reload().await.unwrap();
    let member = snapshot::build(&world.perms, world.member).await.unwrap();
    let admin = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert!(
        member
            .channels
            .iter()
            .all(|channel| channel.name != "secret")
    );
    assert!(
        admin
            .channels
            .iter()
            .any(|channel| channel.name == "secret")
    );
    let event = server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated {
        channel: Some(v1::Channel {
            id: id_to_bytes(secret),
            ..v1::Channel::default()
        }),
    });
    assert!(!snapshot::delivers(&world.perms, world.member, &event));
    assert!(snapshot::delivers(&world.perms, world.admin, &event));
    assert!(snapshot::delivers(
        &world.perms,
        world.member,
        &server_envelope::Msg::RoleCreated(v1::RoleCreated::default())
    ));
}

struct World {
    pool: Arc<db::Pool>,
    perms: Arc<PermissionService>,
    channels: Channels,
    admin: Id,
    member: Id,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let (admin, member) = {
        let mut conn = pool.write().await;
        let admin = insert_user(&mut conn, "admin").await;
        let member = insert_user(&mut conn, "member").await;
        roles::assign(&mut conn, admin, ADMIN_ROLE_ID)
            .await
            .unwrap();
        (admin, member)
    };
    let perms = Arc::new(PermissionService::open(Arc::clone(&pool)).await.unwrap());
    let channels = Channels::new(Arc::clone(&perms));
    World {
        pool,
        perms,
        channels,
        admin,
        member,
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
            password_hash: "hash".into(),
            kind: UserKind::User,
        },
    )
    .await
    .unwrap();
    id
}

fn text_channel(name: &str, parent: Option<Id>, position: i32) -> v1::CreateChannel {
    v1::CreateChannel {
        kind: i32::from(v1::ChannelKind::Text),
        name: name.to_owned(),
        parent_id: parent.map(id_to_bytes),
        position,
        topic: None,
        voice: None,
    }
}

fn invalid_creates() -> Vec<(&'static str, v1::CreateChannel, &'static str)> {
    let mut nested = text_channel("nested", None, 0);
    nested.kind = i32::from(v1::ChannelKind::Category);
    nested.parent_id = Some(id_to_bytes(Id::generate()));
    let mut voiced = text_channel("talk", None, 0);
    voiced.voice = Some(v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::Voice),
        bitrate: None,
        user_limit: None,
    });
    let mut bitrate = text_channel("fast", None, 0);
    bitrate.kind = i32::from(v1::ChannelKind::Voice);
    bitrate.voice = Some(v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::Voice),
        bitrate: Some(1),
        user_limit: None,
    });
    let mut limit = text_channel("crowd", None, 0);
    limit.kind = i32::from(v1::ChannelKind::Voice);
    limit.voice = Some(v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::LowLatency),
        bitrate: Some(
            VoiceProfile::LowLatency
                .encoder_params()
                .default_bitrate_bps,
        ),
        user_limit: Some(100),
    });
    vec![
        ("empty name", text_channel("", None, 0), "invalid"),
        ("nested category", nested, "invalid"),
        ("voice settings on text", voiced, "invalid"),
        ("bitrate out of range", bitrate, "invalid"),
        ("user limit above 99", limit, "invalid"),
        (
            "direct message kind",
            {
                let mut command = text_channel("dm", None, 0);
                command.kind = i32::from(v1::ChannelKind::Dm);
                command
            },
            "invalid",
        ),
    ]
}

fn class(result: Result<handlers::CommandReply, handlers::Error>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(handlers::Error::Forbidden(_)) => "forbidden",
        Err(handlers::Error::NotFound(_)) => "missing",
        Err(handlers::Error::Invalid(_)) => "invalid",
        Err(error) => panic!("{error}"),
    }
}

fn find<'a>(snap: &'a v1::Snapshot, name: &str, kind: v1::ChannelKind) -> &'a v1::Channel {
    snap.channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(kind))
        .unwrap_or_else(|| panic!("missing {name}"))
}

fn id_of(channel: &v1::Channel) -> Id {
    id_from_bytes(&channel.id).unwrap()
}

trait ChannelEvent {
    fn channel_created(&self) -> &v1::Channel;
}

impl ChannelEvent for server_envelope::Msg {
    fn channel_created(&self) -> &v1::Channel {
        let server_envelope::Msg::ChannelCreated(message) = self else {
            panic!("expected ChannelCreated");
        };
        message.channel.as_ref().unwrap()
    }
}
