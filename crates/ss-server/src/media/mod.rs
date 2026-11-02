//! Opaque audio fan-out.
//!
//! [`spike`] serves the unauthenticated voice spike. [`permitted`] is the
//! authenticated router: it still forwards shared `Bytes` without decoding.

mod permitted;
mod spike;
pub mod streams;

pub use permitted::{ForwardOutcome, Forwarder};
pub use spike::{Router, Stats};
