//! HTTPS routes that do not fit a single control frame.
//!
//! History pagination is [`messages`]. The process bind is left to the
//! operations task; tests call [`messages::router`] directly.

pub mod messages;
