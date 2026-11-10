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
    move_role: MoveRole, set_user_roles: SetUserRoles, set_channel_override: SetChannelOverride,
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
    /// Load the next page of older conversations into the state, from
    /// `State::direct_messages_next`. Returns `false` once every
    /// conversation is loaded.
    pub async fn list_dms(&self) -> Result<bool, Error> {
        let (generation, cursor) = {
            let state = self.store.read();
            (state.generation, state.direct_messages_next.clone())
        };
        let Some(cursor) = cursor else {
            return Ok(false);
        };
        let reply = self
            .command(Msg::ListDms(v1::ListDms {
                cursor: Some(cursor),
                limit: 0,
            }))
            .await?;
        let Some(ack::Result::DmPage(page)) = reply.result else {
            return Err(Error::Protocol("expected a DM page"));
        };
        self.store.direct_page(generation, &page)?;
        let _ = self.events.send(crate::ClientEvent::ChannelsChanged);
        Ok(page.next.is_some())
    }

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
    /// End this login session on the server, then stop this client.
    ///
    /// The server deletes the session, so the token stops working for QUIC
    /// and HTTPS, and closes every connection that used it. Whatever the
    /// outcome, this client stops: no reconnect follows, the HTTPS bearer is
    /// dropped and the state reports `Disconnected`. Drop the client after
    /// this. Host storage is not touched; the host forgets the saved token.
    pub async fn logout(&self) -> Result<(), Error> {
        let result = self.command(Msg::Logout(v1::Logout {})).await;
        *self.auth.write() = None;
        self.abort();
        self.store.connection(crate::ConnectionState::Disconnected);
        let _ = self.events.send(crate::ClientEvent::Connection(
            crate::ConnectionState::Disconnected,
        ));
        match result?.result {
            Some(ack::Result::Empty(_)) => Ok(()),
            _ => Err(Error::Protocol("expected empty acknowledgement")),
        }
    }
    /// This account's live sessions on the server, oldest first. `current`
    /// marks the one this client uses.
    pub async fn list_sessions(&self) -> Result<Vec<v1::SessionInfo>, Error> {
        match self
            .command(Msg::ListSessions(v1::ListSessions {}))
            .await?
            .result
        {
            Some(ack::Result::Sessions(list)) => Ok(list.sessions),
            _ => Err(Error::Protocol("expected session list")),
        }
    }
    /// End another of this account's sessions, for example a lost device.
    /// Its connections are closed. Use [`Self::logout`] for this client's own
    /// session: revoking it here also closes this connection, and the
    /// reconnect that follows fails as unauthenticated.
    pub async fn revoke_session(&self, session: Id) -> Result<(), Error> {
        match self
            .command(Msg::RevokeSession(v1::RevokeSession {
                session_id: session.as_bytes().to_vec().into(),
            }))
            .await?
            .result
        {
            Some(ack::Result::Empty(_)) => Ok(()),
            _ => Err(Error::Protocol("expected empty acknowledgement")),
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
