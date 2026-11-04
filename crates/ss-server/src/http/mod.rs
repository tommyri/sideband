//! HTTPS routes that do not fit a single control frame.
//!
//! History pagination is [`messages`]. The audit log is [`audit`]. Uploads and
//! downloads are [`attachments`]. Incoming posts are [`webhooks`]. The public
//! description is [`info`]. Health and metrics are [`internal`]. `run` binds
//! the listeners; tests call routers directly.

pub mod attachments;
pub mod audit;
pub mod hardening;
pub mod info;
pub mod internal;
pub mod invites;
pub mod messages;
pub mod role_admin;
pub mod webhook_admin;
pub mod webhooks;
