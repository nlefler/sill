//! Sill client discovery (M4): Bonjour browsing of `_sill._tcp`, the row policy, connecting, and the
//! reconnect schedule. Pure decisions ([`policy`], [`supervisor::Supervisor`]) are kept apart from I/O.

pub mod browser;
pub mod connect;
pub mod policy;
pub mod supervisor;

pub use browser::{run_browser, Browser, BrowserEvent, FakeBrowser, MdnsBrowser, RowEvent, RowTable};
pub use connect::{connect_by_address, connect_saved, ConnectOutcome, DialConfig};
pub use policy::{classify, HomeDoor, Row, RowClass};
pub use supervisor::{reconnect, Poll, ReconnectEnd, ReconnectInputs, Supervisor};
