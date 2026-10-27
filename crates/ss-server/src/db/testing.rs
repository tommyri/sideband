//! Shared fixtures for repository tests.

use sqlx::SqliteConnection;
use ss_core::{ChannelKind, Id, UserKind};

use super::{
    TestPool,
    channels::{self, NewChannel},
    messages::{self, NewMessage},
    model::{Channel, Message, User},
    test_pool,
    users::{self, NewUser},
};

pub(crate) async fn pool() -> TestPool {
    test_pool().await.expect("temp database")
}

pub(crate) fn id(n: u8) -> Id {
    let mut bytes = [0_u8; 16];
    bytes[0] = 0x10;
    bytes[15] = n;
    Id::from_bytes(bytes)
}

pub(crate) async fn user(conn: &mut SqliteConnection, n: u8) -> User {
    users::insert(
        conn,
        NewUser {
            id: id(n),
            username: format!("user{n}"),
            display_name: format!("User {n}"),
            password_hash: "hash".into(),
            kind: UserKind::User,
        },
    )
    .await
    .expect("user")
}

pub(crate) async fn text_channel(conn: &mut SqliteConnection, n: u8, owner: Id) -> Channel {
    channels::insert(
        conn,
        NewChannel {
            id: id(n),
            kind: ChannelKind::Text,
            name: format!("channel{n}"),
            topic: None,
            parent_id: None,
            position: 0,
            voice_profile: None,
            voice_bitrate: None,
            user_limit: None,
            created_by: owner,
        },
    )
    .await
    .expect("channel")
}

pub(crate) async fn message(
    conn: &mut SqliteConnection,
    n: u8,
    channel_id: Id,
    author_id: Id,
) -> Message {
    messages::insert(
        conn,
        NewMessage {
            id: id(n),
            channel_id,
            author_id: Some(author_id),
            webhook_id: None,
            content: format!("message{n}"),
            reply_to: None,
            mentions: Vec::new(),
            mentions_everyone: false,
        },
    )
    .await
    .expect("message")
}
