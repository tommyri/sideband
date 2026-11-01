//! One authenticated control session against `ss-server run`.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use ss_core::Id;
use ss_net::{
    Connection,
    control::{ClientControl, ControlError, Reply},
    pins::PinStore,
};
use ss_proto::{
    v1::{self, client_envelope, server_envelope},
    validate::id_to_bytes,
};
use tokio::{sync::mpsc, time::timeout};

/// Hello, join, and command replies must arrive within this.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// Where the bots connect.
#[derive(Clone)]
pub struct Target {
    /// Hostname or IP.
    pub host: String,
    /// QUIC port.
    pub port: u16,
    /// Certificate pins, already trusted by the caller.
    pub pins: Arc<dyn PinStore>,
}

/// How a session authenticates.
pub enum Credential<'a> {
    /// A token from an earlier `Welcome`. Skips the login rate limit.
    Token(&'a str),
    /// A new account.
    Register {
        /// Invite code.
        invite: &'a str,
        /// Username.
        username: &'a str,
        /// Password.
        password: &'a str,
    },
    /// Password login. Counts against the per-address login limit.
    Login {
        /// Username.
        username: &'a str,
        /// Password.
        password: &'a str,
    },
}

/// A failure while opening or using a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// QUIC, TLS, or pin failure.
    #[error(transparent)]
    Transport(#[from] ss_net::Error),
    /// The control stream closed or broke.
    #[error(transparent)]
    Control(#[from] ControlError),
    /// The server answered with `Error`.
    #[error("{command} rejected ({code:?}): {message}")]
    Rejected {
        /// Command that failed.
        command: &'static str,
        /// Protocol error code.
        code: v1::ErrorCode,
        /// Server text.
        message: String,
    },
    /// No reply within [`REPLY_TIMEOUT`].
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// Joined without a `TrackAdded` for this connection.
    #[error("joined voice without a track")]
    NoTrack,
}

impl SessionError {
    /// True when the server asked us to slow down.
    pub fn rate_limited(&self) -> bool {
        matches!(
            self,
            Self::Rejected {
                code: v1::ErrorCode::RateLimited,
                ..
            }
        )
    }
}

/// An authenticated connection. Dropping the control handle closes QUIC.
pub struct Session {
    /// Datagram side.
    pub connection: Arc<Connection>,
    /// Command side.
    pub control: Arc<ClientControl>,
    /// Server events with request id zero. Must be drained.
    pub events: mpsc::Receiver<v1::ServerEnvelope>,
    /// Authenticated user id bytes.
    pub user_id: Bytes,
    /// Token for the next Hello.
    pub token: String,
    /// State at Hello.
    pub snapshot: v1::Snapshot,
}

/// Connect and send Hello.
pub async fn open(target: &Target, credential: Credential<'_>) -> Result<Session, SessionError> {
    let mut connection = timeout(
        REPLY_TIMEOUT,
        ss_net::client::connect(&target.host, target.port, Arc::clone(&target.pins)),
    )
    .await
    .map_err(|_| SessionError::Timeout("handshake"))??;
    let stream = connection.open_control().await?;
    let (control, events) = ClientControl::start(stream);
    let auth = match credential {
        Credential::Token(token) => v1::hello::Auth::SessionToken(token.to_owned()),
        Credential::Register {
            invite,
            username,
            password,
        } => v1::hello::Auth::Register(v1::Register {
            invite_code: invite.to_owned(),
            username: username.to_owned(),
            password: password.to_owned(),
        }),
        Credential::Login { username, password } => v1::hello::Auth::Login(v1::Login {
            username: username.to_owned(),
            password: password.to_owned(),
        }),
    };
    let reply = timeout(
        REPLY_TIMEOUT,
        control.request(client_envelope::Msg::Hello(v1::Hello {
            protocol_version: ss_proto::PROTOCOL_VERSION,
            client_name: "ss-cli load".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            auth: Some(auth),
        })),
    )
    .await
    .map_err(|_| SessionError::Timeout("Welcome"))??;
    let welcome = match reply.msg {
        Some(server_envelope::Msg::Welcome(welcome)) => welcome,
        Some(server_envelope::Msg::Error(error)) => {
            return Err(rejected("Hello", &error));
        }
        _ => {
            return Err(SessionError::Control(ControlError::Protocol(
                "expected Welcome",
            )));
        }
    };
    let user_id = welcome.user.map(|user| user.id).unwrap_or_default();
    Ok(Session {
        connection: Arc::new(connection),
        control: Arc::new(control),
        events,
        user_id,
        token: welcome.resume_token,
        snapshot: welcome.snapshot.unwrap_or_default(),
    })
}

impl Session {
    /// Sends one command and returns its `Ack`.
    pub async fn call(
        &self,
        command: &'static str,
        message: client_envelope::Msg,
    ) -> Result<v1::Ack, SessionError> {
        call(&self.control, command, message).await
    }

    /// Joins `channel` and waits for this connection's `TrackAdded`.
    ///
    /// Other events read while waiting are dropped; bots keep no state.
    pub async fn join_voice(&mut self, channel: Id) -> Result<u32, SessionError> {
        self.call(
            "JoinVoice",
            client_envelope::Msg::JoinVoice(v1::JoinVoice {
                channel_id: id_to_bytes(channel),
                self_mute: false,
                self_deaf: false,
            }),
        )
        .await?;
        let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .map_err(|_| SessionError::NoTrack)?
                .ok_or(SessionError::Control(ControlError::Closed))?;
            if let Some(track) = own_track(&event, &self.user_id) {
                return Ok(track);
            }
        }
    }

    /// Leaves voice. The server removes this connection's track.
    pub async fn leave_voice(&self) -> Result<(), SessionError> {
        self.call(
            "LeaveVoice",
            client_envelope::Msg::LeaveVoice(v1::LeaveVoice {}),
        )
        .await
        .map(|_| ())
    }
}

/// [`Session::call`] on a shared control handle.
pub async fn call(
    control: &ClientControl,
    command: &'static str,
    message: client_envelope::Msg,
) -> Result<v1::Ack, SessionError> {
    match timeout(REPLY_TIMEOUT, control.call(message))
        .await
        .map_err(|_| SessionError::Timeout(command))??
    {
        Reply::Ack(ack) => Ok(ack),
        Reply::Error(error) => Err(rejected(command, &error)),
    }
}

/// The track id when `event` announces this user's own track.
pub fn own_track(event: &v1::ServerEnvelope, user_id: &Bytes) -> Option<u32> {
    match &event.msg {
        Some(server_envelope::Msg::TrackAdded(added)) if added.user_id == *user_id => {
            Some(added.track_id)
        }
        _ => None,
    }
}

fn rejected(command: &'static str, error: &v1::Error) -> SessionError {
    SessionError::Rejected {
        command,
        code: v1::ErrorCode::try_from(error.code).unwrap_or(v1::ErrorCode::Unspecified),
        message: error.message.clone(),
    }
}
