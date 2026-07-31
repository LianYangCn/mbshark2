//! Request/response pairing and the entry display model.
//!
//! Each captured event becomes one [`Entry`] (one display block). A logical
//! "transaction" is implied by consecutive entries sharing the same `counter`
//! (e.g. a REQUEST block followed by its RESPONSE block).

pub mod model;
pub mod pairing;

pub use model::{Entry, EntryBody, Tag};
pub use pairing::{on_frame, sweep, PairingState, PendingReq};
