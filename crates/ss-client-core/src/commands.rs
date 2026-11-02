//! Typed command results over the correlated protocol router.
use crate::{Client, Error, state::id};
use ss_core::Id;
use ss_proto::v1::{self, ack, client_envelope::Msg};

macro_rules! empty_commands {
    ($($name:ident: $wire:ident),* $(,)?) => { impl Client { $(
        #[doc = concat!("Send `", stringify!($wire), "` and require an empty acknowledgement.")]
        pub async fn $name(&self, command: v1::$wire) -> Result<(), Error> {
            match self.command(Msg::$wire(command)).await?.result {
                Some(ack::Result::Empty(_)) => Ok(()),
                _ => Err(Error::Protocol("expected empty acknowledgement")),
            }
        }
    )* } };
}
empty_commands! {
    join_voice: JoinVoice, leave_voice: LeaveVoice, set_voice_state: SetVoiceState,
    edit_message: EditMessage, delete_message: DeleteMessage, typing: Typing,
    mark_read: MarkRead, leave_dm: LeaveDm, add_dm_members: AddDmMembers,
    update_channel: UpdateChannel, delete_channel: DeleteChannel, move_channel: MoveChannel,
    revoke_invite: RevokeInvite, kick_user: KickUser, ban_user: BanUser, unban_user: UnbanUser,
    move_user: MoveUser, server_mute_user: ServerMuteUser, delete_role: DeleteRole,
    set_user_roles: SetUserRoles, set_channel_override: SetChannelOverride,
    subscribe_track: SubscribeTrack, unsubscribe_track: UnsubscribeTrack, stop_stream: StopStream,
    request_keyframe: RequestKeyframe, set_preferred_quality: SetPreferredQuality,
    set_presence: SetPresence, delete_webhook: DeleteWebhook,
}
macro_rules! resource_commands {
    ($($name:ident: $wire:ident => $variant:ident, $result:ty),* $(,)?) => { impl Client { $(
        #[doc = concat!("Send `", stringify!($wire), "` and return its typed resource.")]
        pub async fn $name(&self, command: v1::$wire) -> Result<$result, Error> {
            match self.command(Msg::$wire(command)).await?.result {
                Some(ack::Result::$variant(value)) => Ok(value),
                _ => Err(Error::Protocol("unexpected acknowledgement resource")),
            }
        }
    )* } };
}
resource_commands! {
    create_dm: CreateDm => DmChannel, v1::Channel,
    create_invite: CreateInvite => Invite, v1::Invite,
    create_role: CreateRole => Role, v1::Role,
    update_role: UpdateRole => Role, v1::Role,
    update_profile: UpdateProfile => User, v1::User,
    create_webhook: CreateWebhook => Webhook, v1::Webhook,
}
impl Client {
    /// Start publishing a video/screen stream and return its server track id.
    pub async fn start_stream(&self, command: v1::StartStream) -> Result<u32, Error> {
        match self.command(Msg::StartStream(command)).await?.result {
            Some(ack::Result::TrackId(track)) if track != 0 => Ok(track),
            _ => Err(Error::Protocol("expected video track id")),
        }
    }

    /// Send a text message and return its assigned ULID. Server events carry
    /// the stored message and nonce; no optimistic duplicate is inserted.
    pub async fn send_message(&self, command: v1::SendMessage) -> Result<Id, Error> {
        match self.command(Msg::SendMessage(command)).await?.result {
            Some(ack::Result::MessageId(bytes)) => id(&bytes),
            _ => Err(Error::Protocol("expected message id")),
        }
    }
    /// Create a guild channel and return its assigned ULID.
    pub async fn create_channel(&self, command: v1::CreateChannel) -> Result<Id, Error> {
        match self.command(Msg::CreateChannel(command)).await?.result {
            Some(ack::Result::ChannelId(bytes)) => id(&bytes),
            _ => Err(Error::Protocol("expected channel id")),
        }
    }
}
