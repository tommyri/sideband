//! Voice membership.
//!
//! [`spike`] is the unauthenticated T-016 registry. [`session`] is the
//! permission-checked membership used by the authenticated accept loop.

pub(crate) mod rooms;
mod session;
mod spike;

pub(crate) use session::Outcome;
pub use session::Voice;
pub(crate) use spike::CONTROL_CAPACITY;
pub use spike::Registry;
