//! Sill's client transport: the device identity, pinned TLS 1.3, and the session over it.
//!
//! Decision logic ([`policy`]) is kept apart from I/O ([`session`]) so it is unit-tested without
//! sockets. Pairing (M3) reuses [`tls::client_config`] with [`tls::PinMode::TrustOnFirstUse`] and
//! [`tls::Alpn::Pairing`].

pub mod addr;
pub mod b64;
pub mod identity;
pub mod macinfo;
pub mod pairing;
pub mod pin;
pub mod policy;
pub mod safetext;
pub mod saved;
pub mod session;
pub mod tls;

pub use identity::{Fingerprint, Identity};
pub use macinfo::MacInfoCheck;
pub use pairing::{PairError, PairLink, PairOptions};
pub use saved::{SavedMac, SavedMacs, Standing};
pub use session::{connect, CloseReason, Command, ConnectOptions, Event, Session, SessionConfig, SessionHandle};
pub use tls::{Alpn, PinMode};
