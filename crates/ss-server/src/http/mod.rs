//! HTTPS routes that do not fit a single control frame.
//!
//! History pagination is [`messages`]. The audit log is [`audit`]. Uploads and
//! downloads are [`attachments`]. Incoming posts are [`webhooks`]. The process
//! bind is left to the operations task; tests call the routers directly.

pub mod attachments;
pub mod audit;
pub mod messages;
pub mod webhooks;
