//! Pure presentation logic without DOM access – tested natively with
//! `cargo test` (replaces the former Vitest suite).
pub mod b64;
pub mod branch;
pub mod budget;
pub mod costs;
pub mod diff;
pub mod env;
pub mod errors;
pub mod format;
pub mod labels;
pub mod limits;
pub mod pr;
pub mod progress;
pub mod protection;
pub mod shortcuts;
pub mod task;
