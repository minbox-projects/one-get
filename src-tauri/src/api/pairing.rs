//! Pairing code generation, bearer-token generation, and secret hashing.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Number of random bytes in a bearer token.
pub const TOKEN_BYTES: usize = 32;

/// Digits in a pairing code.
pub const PAIRING_CODE_DIGITS: usize = 8;

/// Lifetime of a pairing code.
pub const CODE_TTL_MS: i64 = 600_000;

/// Duration pairing stays locked after too many failed attempts.
pub const LOCKOUT_MS: i64 = 300_000;

/// Failed attempts that trigger a lockout.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;

/// Minimum interval between persisted last-used updates.
pub const LAST_USED_UPDATE_INTERVAL_MS: i64 = 60_000;

const PAIRING_CODE_RANGE: u32 = 100_000_000;

/// Generate a uniformly distributed eight-digit pairing code.
pub fn generate_pairing_code() -> String {
    format!("{:08}", rand::random_range(0..PAIRING_CODE_RANGE))
}

/// Generate a 256-bit bearer token as 64 lowercase hexadecimal characters.
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}

/// Hash a secret to a lowercase hexadecimal SHA-256 digest.
pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Compare a secret against a stored hash in constant time.
pub fn verify_secret_hash(secret: &str, expected_hash: &str) -> bool {
    let actual = hash_secret(secret);
    bool::from(actual.as_bytes().ct_eq(expected_hash.as_bytes()))
}
