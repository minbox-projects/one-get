//! Loopback browser-integration primitives: pairing, capture protocol, errors,
//! clock, and request admission limits.

pub mod auth;
pub mod capture;
pub mod clock;
pub mod desktop;
pub mod error;
pub mod pairing;
pub mod pairing_store;
pub mod protocol;
pub mod rate_limit;
pub mod routes;
pub mod server;
