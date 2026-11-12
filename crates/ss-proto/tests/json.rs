//! Canonical protobuf JSON for the bot API (`json` feature).
#![cfg(feature = "json")]

use ss_proto::v1::{self, server_envelope::Msg};

#[test]
fn server_events_use_the_canonical_protobuf_json_mapping() {
    let envelope = v1::ServerEnvelope {
        request_id: 0,
        msg: Some(Msg::MessageCreated(v1::MessageCreated {
            message: Some(v1::Message {
                id: vec![1_u8; 16].into(),
                channel_id: vec![2_u8; 16].into(),
                author: Some(v1::User {
                    id: vec![3_u8; 16].into(),
                    username: "dice".into(),
                    display_name: "Dice".into(),
                    avatar_attachment_id: None,
                    kind: v1::UserKind::Bot.into(),
                }),
                content: "/roll 2d6".into(),
                created_at: 1_700_000_000_000,
                ..Default::default()
            }),
            nonce: None,
        })),
    };
    let json = serde_json::to_value(&envelope).unwrap();
    let message = &json["messageCreated"]["message"];
    assert_eq!(json.get("requestId"), None, "defaults are omitted");
    assert_eq!(message["id"], "AQEBAQEBAQEBAQEBAQEBAQ==");
    assert_eq!(message["createdAt"], "1700000000000", "int64 is a string");
    assert_eq!(message["author"]["kind"], "USER_KIND_BOT");
    assert_eq!(message["author"]["displayName"], "Dice");
    let back: v1::ServerEnvelope = serde_json::from_value(json).unwrap();
    assert_eq!(back, envelope);
}

#[test]
fn unknown_fields_are_ignored_on_input() {
    let parsed: v1::SendMessage =
        serde_json::from_str(r#"{"content":"hi","fromTheFuture":1}"#).unwrap();
    assert_eq!(parsed.content, "hi");
}
