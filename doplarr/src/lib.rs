//! Doplarr as a library: the config, startup, provider, and Discord layers.
//!
//! The production binary (`src/main.rs`) and the dev-only canary driver
//! (`src/bin/chaptarr_canary.rs`, feature `canary`) both build on this crate
//! so the canary constructs its backends through the exact production path.

pub mod args;
pub mod config;
pub mod discord;
pub mod providers;
pub mod startup;
