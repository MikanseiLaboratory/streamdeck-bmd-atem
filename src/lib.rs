//! Stream Deck plugin library for Blackmagic ATEM control.
//!
//! The binary and the TypeScript contract generator both link this crate so
//! `inventory` registrations for actions and settings stay in the build.

pub mod actions;
pub mod availability;
pub mod contracts;
pub mod pool;
pub mod runtime;
