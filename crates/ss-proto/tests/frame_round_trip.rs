//! Random and exhaustive framing round-trips for both envelopes.
//!
//! `encode` then `decode`, and the same bytes through `read_frame`.

use bytes::Bytes;
use proptest::prelude::*;
use ss_proto::{
    decode, encode, read_frame,
    v1::{
        Ack, AddDmMembers, Attachment, BanUser, Banned, Channel, ChannelCreated, ChannelDeleted,
        ChannelUpdated, ClientEnvelope, Codec, CreateChannel, CreateDm, CreateInvite, CreateRole,
        DeleteChannel, DeleteMessage, DeleteRole, EditMessage, Error, Hello, Invite, InviteCreated,
        InviteRevoked, JoinVoice, KickUser, Kicked, LeaveDm, LeaveVoice, LinkPreview, Login,
        MarkRead, Member, MemberJoined, MemberLeft, MemberRolesUpdated, MemberUpdated, Message,
        MessageCreated, MessageDeleted, MessageUpdated, MoveChannel, MoveUser, Ping, Pong,
        Presence, PresenceUpdated, ReadState, ReadStateUpdated, Register, RevokeInvite, Role,
        RoleCreated, RoleDeleted, RoleUpdated, SendMessage, ServerEnvelope, ServerInfo,
        ServerMuteUser, ServerNotice, SetChannelOverride, SetPreferredQuality, SetPresence,
        SetUserRoles, SetVoiceState, Snapshot, SubscribeTrack, Track, TrackAdded, TrackRemoved,
        Typing, TypingStarted, UnbanUser, UnsubscribeTrack, UpdateChannel, UpdateProfile,
        UpdateRole, User, VoiceSettings, VoiceState, VoiceStateUpdated, Welcome, ack,
        client_envelope, hello, server_envelope,
    },
};

#[derive(Clone, Debug)]
struct Sample {
    pick: u32,
    extra: u32,
    bits: u64,
    flag: bool,
    text: String,
    text2: String,
    blob: Bytes,
    blob2: Bytes,
    blobs: Vec<Bytes>,
    time: i64,
}

fn arb_bytes() -> impl Strategy<Value = Bytes> {
    proptest::collection::vec(any::<u8>(), 0..=16).prop_map(Bytes::from)
}

fn arb_string() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::char::range('a', 'z'), 0..=12)
        .prop_map(|chars| chars.into_iter().collect())
}

prop_compose! {
    fn arb_sample()(
        pick in any::<u32>(),
        extra in any::<u32>(),
        bits in any::<u64>(),
        flag in any::<bool>(),
        text in arb_string(),
        text2 in arb_string(),
        blob in arb_bytes(),
        blob2 in arb_bytes(),
        blobs in proptest::collection::vec(arb_bytes(), 0..=2),
        time in any::<i64>(),
    ) -> Sample {
        Sample {
            pick,
            extra,
            bits,
            flag,
            text,
            text2,
            blob,
            blob2,
            blobs,
            time,
        }
    }
}

fn code(n: u32, modulo: u32) -> i32 {
    i32::try_from(n % modulo).unwrap_or(0)
}

fn opt_blob(sample: &Sample) -> Option<Bytes> {
    sample.flag.then(|| sample.blob.clone())
}

fn user(sample: &Sample) -> User {
    User {
        id: sample.blob.clone(),
        username: sample.text.clone(),
        display_name: sample.text2.clone(),
        avatar_attachment_id: opt_blob(sample),
        kind: code(sample.pick, 3),
    }
}

fn voice(sample: &Sample) -> Option<VoiceSettings> {
    sample.flag.then(|| VoiceSettings {
        profile: code(sample.pick, 4),
        bitrate: sample.flag.then_some(sample.pick),
        user_limit: sample.flag.then_some(sample.extra),
    })
}

fn channel(sample: &Sample) -> Channel {
    Channel {
        effective_permissions: Some(ss_core::Permissions::EVERYONE.bits()),
        id: sample.blob.clone(),
        kind: code(sample.pick, 6),
        name: sample.text.clone(),
        topic: sample.flag.then(|| sample.text2.clone()),
        parent_id: opt_blob(sample),
        position: code(sample.extra, 10_000),
        voice: voice(sample),
        created_by: sample.flag.then(|| sample.blob2.clone()),
        created_at: sample.time,
        member_ids: sample.blobs.clone(),
        last_message_id: sample.flag.then(|| sample.blob.clone()),
    }
}

fn role(sample: &Sample) -> Role {
    Role {
        id: sample.blob.clone(),
        name: sample.text.clone(),
        color: sample.flag.then_some(sample.pick),
        permissions: sample.bits,
        position: code(sample.extra, 10_000),
        is_default: sample.flag,
    }
}

fn invite(sample: &Sample) -> Invite {
    Invite {
        id: sample.blob.clone(),
        code: sample.text.clone(),
        created_by: sample.blob2.clone(),
        max_uses: sample.flag.then_some(sample.pick),
        uses: sample.extra,
        expires_at: sample.flag.then_some(sample.time),
        created_at: sample.time,
        revoked_at: sample.flag.then_some(sample.time),
    }
}

fn attachment(sample: &Sample) -> Attachment {
    Attachment {
        id: sample.blob.clone(),
        filename: sample.text.clone(),
        mime: sample.text2.clone(),
        size: sample.bits,
        width: sample.flag.then_some(sample.pick),
        height: sample.flag.then_some(sample.extra),
    }
}

fn link_preview(sample: &Sample) -> LinkPreview {
    LinkPreview {
        url: sample.text.clone(),
        title: sample.flag.then(|| sample.text2.clone()),
        description: sample.flag.then(|| sample.text.clone()),
        image_url: sample.flag.then(|| sample.text2.clone()),
        site_name: sample.flag.then(|| sample.text.clone()),
    }
}

fn message(sample: &Sample) -> Message {
    Message {
        id: sample.blob.clone(),
        channel_id: sample.blob2.clone(),
        author: sample.flag.then(|| user(sample)),
        content: sample.text.clone(),
        reply_to: opt_blob(sample),
        attachments: if sample.flag {
            vec![attachment(sample)]
        } else {
            Vec::new()
        },
        mentions: sample.blobs.clone(),
        link_previews: if sample.flag {
            vec![link_preview(sample)]
        } else {
            Vec::new()
        },
        created_at: sample.time,
        edited_at: sample.flag.then_some(sample.time),
        webhook_id: sample.flag.then(|| sample.blob2.clone()),
        webhook_name: sample.flag.then(|| sample.text.clone()),
    }
}

fn member(sample: &Sample) -> Member {
    Member {
        user: Some(user(sample)),
        roles: sample.blobs.clone(),
        presence: Some(Presence {
            status: code(sample.pick, 5),
            activity: sample.text.clone(),
        }),
    }
}

fn voice_state(sample: &Sample) -> VoiceState {
    VoiceState {
        user_id: sample.blob.clone(),
        conn_id: sample.blob2.clone(),
        channel_id: opt_blob(sample),
        self_mute: sample.flag,
        self_deaf: sample.pick.is_multiple_of(2),
        server_mute: sample.extra.is_multiple_of(2),
    }
}

fn codec(sample: &Sample) -> Codec {
    Codec {
        name: sample.text.clone(),
        sample_rate: sample.pick,
        channels: sample.extra,
        description: sample.blob.clone(),
    }
}

fn track(sample: &Sample) -> Track {
    Track {
        track_id: sample.pick,
        user_id: sample.blob.clone(),
        conn_id: sample.blob2.clone(),
        kind: code(sample.pick, 4),
        codec: Some(codec(sample)),
        channel_id: sample.blob.clone(),
    }
}

fn read_state(sample: &Sample) -> ReadState {
    ReadState {
        channel_id: sample.blob.clone(),
        last_read_message_id: sample.blob2.clone(),
        mention_count: sample.pick,
    }
}

fn ack_result(sample: &Sample) -> Option<ack::Result> {
    match sample.pick % 9 {
        0 => None,
        1 => Some(ack::Result::Empty(ss_proto::v1::Empty {})),
        2 => Some(ack::Result::MessageId(sample.blob.clone())),
        3 => Some(ack::Result::ChannelId(sample.blob.clone())),
        4 => Some(ack::Result::Invite(invite(sample))),
        5 => Some(ack::Result::DmChannel(channel(sample))),
        6 => Some(ack::Result::Role(role(sample))),
        7 => Some(ack::Result::User(user(sample))),
        _ => Some(ack::Result::Webhook(ss_proto::v1::Webhook {
            id: sample.blob.clone(),
            channel_id: sample.blob2.clone(),
            name: sample.text.clone(),
            token: sample.flag.then(|| sample.text2.clone()),
            created_by: sample.blob.clone(),
            created_at: sample.time,
        })),
    }
}

fn client_envelope(selector: u32, sample: &Sample) -> ClientEnvelope {
    ClientEnvelope {
        request_id: sample.pick,
        msg: client_msg(selector, sample),
    }
}

fn client_msg(selector: u32, sample: &Sample) -> Option<client_envelope::Msg> {
    let which = selector % 39;
    if which == 0 {
        return None;
    }
    let msg = if which <= 16 {
        client_msg_low(which, sample)
    } else {
        client_msg_high(which, sample)
    };
    Some(msg)
}

fn client_msg_low(which: u32, sample: &Sample) -> client_envelope::Msg {
    match which {
        1 => client_envelope::Msg::Hello(Hello {
            protocol_version: sample.pick,
            client_name: sample.text.clone(),
            client_version: sample.text2.clone(),
            os: sample.text.clone(),
            auth: hello_auth(sample),
        }),
        2 => client_envelope::Msg::Ping(Ping {
            sent_at: sample.time,
        }),
        3 => client_envelope::Msg::JoinVoice(JoinVoice {
            channel_id: sample.blob.clone(),
            self_mute: sample.flag,
            self_deaf: sample.pick.is_multiple_of(2),
        }),
        4 => client_envelope::Msg::LeaveVoice(LeaveVoice {}),
        5 => client_envelope::Msg::SetVoiceState(SetVoiceState {
            self_mute: sample.flag,
            self_deaf: sample.extra.is_multiple_of(2),
        }),
        6 => client_envelope::Msg::SendMessage(SendMessage {
            channel_id: sample.blob.clone(),
            content: sample.text.clone(),
            reply_to: opt_blob(sample),
            attachment_ids: sample.blobs.clone(),
            nonce: sample.flag.then(|| sample.text2.clone()),
        }),
        7 => client_envelope::Msg::EditMessage(EditMessage {
            message_id: sample.blob.clone(),
            content: sample.text.clone(),
        }),
        8 => client_envelope::Msg::DeleteMessage(DeleteMessage {
            message_id: sample.blob.clone(),
        }),
        9 => client_envelope::Msg::Typing(Typing {
            channel_id: sample.blob.clone(),
        }),
        10 => client_envelope::Msg::MarkRead(MarkRead {
            channel_id: sample.blob.clone(),
            message_id: sample.blob2.clone(),
        }),
        11 => client_envelope::Msg::CreateDm(CreateDm {
            user_ids: sample.blobs.clone(),
        }),
        12 => client_envelope::Msg::CreateChannel(CreateChannel {
            kind: code(sample.pick, 6),
            name: sample.text.clone(),
            parent_id: opt_blob(sample),
            position: code(sample.extra, 10_000),
            topic: sample.flag.then(|| sample.text2.clone()),
            voice: voice(sample),
        }),
        13 => client_envelope::Msg::UpdateChannel(UpdateChannel {
            channel_id: sample.blob.clone(),
            kind: sample.flag.then_some(code(sample.pick, 6)),
            name: sample.flag.then(|| sample.text.clone()),
            parent_id: opt_blob(sample),
            position: sample.flag.then_some(code(sample.extra, 10_000)),
            topic: sample.flag.then(|| sample.text2.clone()),
            voice: voice(sample),
        }),
        14 => client_envelope::Msg::DeleteChannel(DeleteChannel {
            channel_id: sample.blob.clone(),
        }),
        15 => client_envelope::Msg::MoveChannel(MoveChannel {
            channel_id: sample.blob.clone(),
            parent_id: opt_blob(sample),
            position: code(sample.extra, 10_000),
        }),
        _ => client_envelope::Msg::CreateInvite(CreateInvite {
            max_uses: sample.flag.then_some(sample.pick),
            expires_at: sample.flag.then_some(sample.time),
        }),
    }
}

fn client_msg_high(which: u32, sample: &Sample) -> client_envelope::Msg {
    match which {
        17 => client_envelope::Msg::RevokeInvite(RevokeInvite {
            invite_id: sample.blob.clone(),
        }),
        18 => client_envelope::Msg::KickUser(KickUser {
            user_id: sample.blob.clone(),
            reason: sample.text.clone(),
        }),
        19 => client_envelope::Msg::BanUser(BanUser {
            user_id: sample.blob.clone(),
            reason: sample.text.clone(),
            expires_at: sample.flag.then_some(sample.time),
        }),
        20 => client_envelope::Msg::UnbanUser(UnbanUser {
            user_id: sample.blob.clone(),
        }),
        21 => client_envelope::Msg::MoveUser(MoveUser {
            user_id: sample.blob.clone(),
            channel_id: sample.blob2.clone(),
        }),
        22 => client_envelope::Msg::ServerMuteUser(ServerMuteUser {
            user_id: sample.blob.clone(),
            muted: sample.flag,
        }),
        23 => client_envelope::Msg::CreateRole(CreateRole {
            name: sample.text.clone(),
            color: sample.flag.then_some(sample.pick),
            permissions: sample.bits,
            position: code(sample.extra, 10_000),
        }),
        24 => client_envelope::Msg::UpdateRole(UpdateRole {
            role_id: sample.blob.clone(),
            name: sample.flag.then(|| sample.text.clone()),
            color: sample.flag.then_some(sample.pick),
            permissions: sample.flag.then_some(sample.bits),
            position: sample.flag.then_some(code(sample.extra, 10_000)),
            clear_color: sample.flag,
        }),
        25 => client_envelope::Msg::DeleteRole(DeleteRole {
            role_id: sample.blob.clone(),
        }),
        26 => client_envelope::Msg::SetUserRoles(SetUserRoles {
            user_id: sample.blob.clone(),
            role_ids: sample.blobs.clone(),
        }),
        27 => client_envelope::Msg::SetChannelOverride(SetChannelOverride {
            channel_id: sample.blob.clone(),
            role_id: sample.blob2.clone(),
            allow: sample.bits,
            deny: sample.bits ^ u64::from(sample.pick),
        }),
        28 => client_envelope::Msg::UpdateProfile(UpdateProfile {
            display_name: sample.text.clone(),
            avatar_attachment_id: opt_blob(sample),
        }),
        29 => client_envelope::Msg::SubscribeTrack(SubscribeTrack {
            track_id: sample.pick,
        }),
        30 => client_envelope::Msg::UnsubscribeTrack(UnsubscribeTrack {
            track_id: sample.pick,
        }),
        31 => client_envelope::Msg::RequestKeyframe(ss_proto::v1::RequestKeyframe {
            track_id: sample.pick,
        }),
        33 => client_envelope::Msg::SetPresence(SetPresence {
            status: code(sample.pick, 5),
            activity: sample.text.clone(),
        }),
        34 => client_envelope::Msg::LeaveDm(LeaveDm {
            channel_id: sample.blob.clone(),
        }),
        35 => client_envelope::Msg::AddDmMembers(AddDmMembers {
            channel_id: sample.blob.clone(),
            user_ids: sample.blobs.clone(),
        }),
        36 => client_envelope::Msg::CreateWebhook(ss_proto::v1::CreateWebhook {
            channel_id: sample.blob.clone(),
            name: sample.text.clone(),
        }),
        37 => client_envelope::Msg::DeleteWebhook(ss_proto::v1::DeleteWebhook {
            webhook_id: sample.blob.clone(),
        }),
        38 => client_envelope::Msg::MoveRole(ss_proto::v1::MoveRole {
            role_id: sample.blob.clone(),
            before_role_id: opt_blob(sample),
        }),
        _ => client_envelope::Msg::SetPreferredQuality(SetPreferredQuality {
            track_id: sample.pick,
            max_height: sample.extra,
            max_fps: sample.pick,
        }),
    }
}

fn hello_auth(sample: &Sample) -> Option<hello::Auth> {
    match sample.pick % 5 {
        0 => None,
        1 => Some(hello::Auth::SessionToken(sample.text.clone())),
        2 => Some(hello::Auth::ResumeToken(sample.text2.clone())),
        3 => Some(hello::Auth::Login(Login {
            username: sample.text.clone(),
            password: sample.text2.clone(),
        })),
        _ => Some(hello::Auth::Register(Register {
            invite_code: sample.text.clone(),
            username: sample.text2.clone(),
            password: sample.text.clone(),
        })),
    }
}

fn server_envelope(selector: u32, sample: &Sample) -> ServerEnvelope {
    ServerEnvelope {
        request_id: sample.pick,
        msg: server_msg(selector, sample),
    }
}

fn server_msg(selector: u32, sample: &Sample) -> Option<server_envelope::Msg> {
    let which = selector % 29;
    if which == 0 {
        return None;
    }
    let msg = if which <= 14 {
        server_msg_low(which, sample)
    } else {
        server_msg_high(which, sample)
    };
    Some(msg)
}

fn server_msg_low(which: u32, sample: &Sample) -> server_envelope::Msg {
    match which {
        1 => server_envelope::Msg::Welcome(Welcome {
            user: Some(user(sample)),
            server_info: Some(ServerInfo {
                name: sample.text.clone(),
                version: sample.text2.clone(),
                features: if sample.flag {
                    vec![sample.text.clone()]
                } else {
                    Vec::new()
                },
            }),
            resume_token: sample.text.clone(),
            snapshot: Some(snapshot(sample)),
        }),
        2 => server_envelope::Msg::Ack(Ack {
            request_id: sample.pick,
            result: ack_result(sample),
        }),
        3 => server_envelope::Msg::Error(Error {
            request_id: sample.pick,
            code: code(sample.pick, 10),
            message: sample.text.clone(),
            min_version: sample.flag.then_some(sample.pick),
            max_version: sample.flag.then_some(sample.extra),
            retry_after_ms: sample.flag.then_some(sample.pick),
        }),
        4 => server_envelope::Msg::Pong(Pong {
            sent_at: sample.time,
            server_time: sample.time,
        }),
        5 => server_envelope::Msg::ChannelCreated(ChannelCreated {
            channel: Some(channel(sample)),
        }),
        6 => server_envelope::Msg::ChannelUpdated(ChannelUpdated {
            channel: Some(channel(sample)),
        }),
        7 => server_envelope::Msg::ChannelDeleted(ChannelDeleted {
            channel: Some(channel(sample)),
        }),
        8 => server_envelope::Msg::MemberJoined(MemberJoined {
            member: Some(member(sample)),
        }),
        9 => server_envelope::Msg::MemberLeft(MemberLeft {
            member: Some(member(sample)),
        }),
        10 => server_envelope::Msg::MemberUpdated(MemberUpdated {
            member: Some(member(sample)),
        }),
        11 => server_envelope::Msg::PresenceUpdated(PresenceUpdated {
            user_id: sample.blob.clone(),
            status: code(sample.pick, 5),
            activity: sample.text.clone(),
        }),
        12 => server_envelope::Msg::VoiceStateUpdated(VoiceStateUpdated {
            user_id: sample.blob.clone(),
            conn_id: sample.blob2.clone(),
            channel_id: opt_blob(sample),
            self_mute: sample.flag,
            self_deaf: sample.pick.is_multiple_of(2),
            server_mute: sample.extra.is_multiple_of(2),
        }),
        13 => server_envelope::Msg::TrackAdded(TrackAdded {
            track_id: sample.pick,
            user_id: sample.blob.clone(),
            conn_id: sample.blob2.clone(),
            kind: code(sample.pick, 4),
            codec: Some(codec(sample)),
            channel_id: sample.blob.clone(),
        }),
        _ => server_envelope::Msg::TrackRemoved(TrackRemoved {
            track_id: sample.pick,
        }),
    }
}

fn server_msg_high(which: u32, sample: &Sample) -> server_envelope::Msg {
    match which {
        15 => server_envelope::Msg::MessageCreated(MessageCreated {
            message: Some(message(sample)),
            nonce: sample.flag.then(|| sample.text2.clone()),
        }),
        16 => server_envelope::Msg::MessageUpdated(MessageUpdated {
            message: Some(message(sample)),
            nonce: sample.flag.then(|| sample.text2.clone()),
        }),
        17 => server_envelope::Msg::MessageDeleted(MessageDeleted {
            message: Some(message(sample)),
            nonce: sample.flag.then(|| sample.text2.clone()),
        }),
        18 => server_envelope::Msg::TypingStarted(TypingStarted {
            channel_id: sample.blob.clone(),
            user_id: sample.blob2.clone(),
        }),
        19 => server_envelope::Msg::ReadStateUpdated(ReadStateUpdated {
            channel_id: sample.blob.clone(),
            last_read_message_id: sample.blob2.clone(),
            mention_count: sample.pick,
        }),
        20 => server_envelope::Msg::RoleCreated(RoleCreated {
            role: Some(role(sample)),
        }),
        21 => server_envelope::Msg::RoleUpdated(RoleUpdated {
            role: Some(role(sample)),
        }),
        22 => server_envelope::Msg::RoleDeleted(RoleDeleted {
            role: Some(role(sample)),
        }),
        23 => server_envelope::Msg::MemberRolesUpdated(MemberRolesUpdated {
            user_id: sample.blob.clone(),
            role_ids: sample.blobs.clone(),
        }),
        24 => server_envelope::Msg::InviteCreated(InviteCreated {
            invite: Some(invite(sample)),
        }),
        25 => server_envelope::Msg::InviteRevoked(InviteRevoked {
            invite: Some(invite(sample)),
        }),
        26 => server_envelope::Msg::Kicked(Kicked {
            reason: sample.text.clone(),
            expires_at: sample.flag.then_some(sample.time),
        }),
        27 => server_envelope::Msg::Banned(Banned {
            reason: sample.text.clone(),
            expires_at: sample.flag.then_some(sample.time),
        }),
        _ => server_envelope::Msg::ServerNotice(ServerNotice {
            text: sample.text.clone(),
        }),
    }
}

fn snapshot(sample: &Sample) -> Snapshot {
    let channels = if sample.flag {
        vec![channel(sample)]
    } else {
        Vec::new()
    };
    Snapshot {
        channels: channels.clone(),
        roles: if sample.flag {
            vec![role(sample)]
        } else {
            Vec::new()
        },
        members: if sample.flag {
            vec![member(sample)]
        } else {
            Vec::new()
        },
        voice_states: if sample.flag {
            vec![voice_state(sample)]
        } else {
            Vec::new()
        },
        tracks: if sample.flag {
            vec![track(sample)]
        } else {
            Vec::new()
        },
        read_states: if sample.flag {
            vec![read_state(sample)]
        } else {
            Vec::new()
        },
        direct_messages: channels,
    }
}

fn fixed_sample(flag: bool) -> Sample {
    Sample {
        pick: 3,
        extra: 4,
        bits: 1 << 9,
        flag,
        text: "alpha".to_owned(),
        text2: "beta".to_owned(),
        blob: Bytes::from(vec![1; 16]),
        blob2: Bytes::from(vec![2; 4]),
        blobs: vec![Bytes::from(vec![3, 4]), Bytes::from_static(&[5])],
        time: 1_700_000_000_000,
    }
}

fn assert_sync_round_trip<M>(message: &M)
where
    M: prost::Message + Default + PartialEq + std::fmt::Debug,
{
    let frame = encode(message).expect("frame");
    let decoded: M = decode(frame.as_ref()).expect("decode");
    assert_eq!(&decoded, message);
}

async fn assert_async_round_trip<M>(message: &M)
where
    M: prost::Message + Default + PartialEq + std::fmt::Debug,
{
    let frame = encode(message).expect("frame");
    let (mut writer, mut reader) = tokio::io::duplex(frame.len() + 64);
    tokio::io::AsyncWriteExt::write_all(&mut writer, &frame)
        .await
        .expect("write");
    drop(writer);
    let got: M = read_frame(&mut reader).await.expect("read");
    assert_eq!(&got, message);
}

#[test]
fn every_variant_round_trips() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    for flag in [false, true] {
        let sample = fixed_sample(flag);
        for selector in 0..39 {
            let message = client_envelope(selector, &sample);
            assert_sync_round_trip(&message);
            runtime.block_on(assert_async_round_trip(&message));
        }
        for selector in 0..29 {
            let message = server_envelope(selector, &sample);
            assert_sync_round_trip(&message);
            runtime.block_on(assert_async_round_trip(&message));
        }
    }
}

proptest! {
    #[test]
    fn random_envelopes_round_trip(
        selector in any::<u32>(),
        sample in arb_sample(),
    ) {
        let client = client_envelope(selector, &sample);
        let server = server_envelope(selector, &sample);
        let client_frame = encode(&client).unwrap();
        let server_frame = encode(&server).unwrap();
        let client_decoded: ClientEnvelope = decode(&client_frame).unwrap();
        let server_decoded: ServerEnvelope = decode(&server_frame).unwrap();
        prop_assert_eq!(client_decoded, client);
        prop_assert_eq!(server_decoded, server);
        prop_assert!(client_frame.len() <= ss_proto::MAX_FRAME_LEN + ss_proto::HEADER_LEN);
        prop_assert!(server_frame.len() <= ss_proto::MAX_FRAME_LEN + ss_proto::HEADER_LEN);
    }
}
