//! Deterministic byte generators for engine tests.

/// Generate deterministic bytes of `len` using a simple repeating pattern seeded by `seed`.
pub fn pattern_bytes(seed: u8, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut value = seed;
    for _ in 0..len {
        out.push(value);
        value = value.wrapping_mul(31).wrapping_add(17);
    }
    out
}
