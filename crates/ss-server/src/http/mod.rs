//! HTTPS routes that do not fit a single control frame.
//!
//! History pagination is [`messages`]. Uploads and downloads are
//! [`attachments`]. The process bind is left to the operations task; tests
//! call the routers directly.

pub mod attachments;
pub mod messages;
