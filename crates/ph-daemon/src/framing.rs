//! Private-frame codec re-export. The codec is the shared worker-socket wire
//! contract (served by this crate's workers, spoken by direct-attach clients
//! in ph-tui/ph-cli), so it lives in [`ph_types::daemon::framing`].

pub use ph_types::daemon::framing::*;
