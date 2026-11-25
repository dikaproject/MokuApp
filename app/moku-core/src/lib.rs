//! Shared by `moku` (island), `moku-settings`, `mokuctl` and `moku-setup`: the config schema,
//! data paths, and the IPC protocol. Plain Rust, unit-testable; the only Windows API in here is
//! the user-SID code of the pipe client (`winsid`), the process-name lookups of `apps`, the
//! locale name of `locale` and, for "Tanya AI", Credential Manager (`secrets`) and CNG / DPAPI
//! (`aimem::crypt`), which the island and the settings app share.

pub mod agent_rules;
pub mod ai;
pub mod aimem;
pub mod apps;
pub mod audiofile;
pub mod color;
pub mod config;
pub mod geo;
pub mod hotkey;
pub mod install;
pub mod ipc;
pub mod locale;
pub mod paths;
pub mod release;
pub mod secrets;
pub mod winsid;

pub use config::{Config, Warning};

/// Version string shared by all binaries.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
