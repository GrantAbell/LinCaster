pub mod config;
pub mod error;
pub mod hid;
pub mod state_dump;
pub mod storage;

pub use config::*;
pub use error::*;

// ── Device ID constants ──────────────────────────────────────────────

/// RØDE Microphones USB vendor ID.
pub const RODE_VENDOR_ID: u16 = 0x19F7;

/// RØDECaster Duo USB product ID.
pub const RODECASTER_DUO_PID: u16 = 0x0079;

/// RØDECaster Pro II USB product ID.
pub const RODECASTER_PRO_II_PID: u16 = 0x0078;
