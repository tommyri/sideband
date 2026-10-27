//! `SetPresence`. Offline is derived from the connection registry.

use std::sync::Arc;

use ss_core::Id;
use ss_proto::v1::{self, ack};

use super::{CommandReply, Error};
use crate::registry::ConnectionRegistry;

/// Activity text cap, in Unicode scalar values.
const MAX_ACTIVITY: usize = 128;

/// Presence commands for the authenticated control stream.
pub struct Presence {
    registry: Arc<ConnectionRegistry>,
}

impl Presence {
    pub(crate) fn new(registry: Arc<ConnectionRegistry>) -> Self {
        Self { registry }
    }

    /// `Some` when `msg` is `SetPresence`.
    pub fn dispatch(
        &self,
        user: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        let v1::client_envelope::Msg::SetPresence(command) = msg else {
            return None;
        };
        Some(self.set(user, command))
    }

    fn set(&self, user: Id, command: &v1::SetPresence) -> Result<CommandReply, Error> {
        let status = match v1::PresenceStatus::try_from(command.status) {
            Ok(v1::PresenceStatus::Online | v1::PresenceStatus::Idle | v1::PresenceStatus::Dnd) => {
                command.status
            }
            _ => {
                return Err(Error::Invalid(
                    "presence status must be online, idle, or dnd",
                ));
            }
        };
        if command.activity.chars().count() > MAX_ACTIVITY {
            return Err(Error::Invalid("activity must be at most 128 characters"));
        }
        let event = self
            .registry
            .set_status(user, status, command.activity.clone())
            .map_err(Error::Invalid)?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty {}),
            events: event.into_iter().collect(),
        })
    }
}
