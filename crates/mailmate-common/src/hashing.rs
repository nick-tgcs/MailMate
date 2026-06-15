//! A small, dependency-free **stable** hash used for content identity that must survive
//! across runs, processes, and platforms.
//!
//! `std`'s `DefaultHasher` is explicitly *not* stable across releases and is seeded, so it
//! cannot back an `example_ids_hash` stored in the database or a deterministic
//! train/validation/test split. We use FNV-1a (64-bit) here instead: a fixed,
//! well-specified algorithm with no seed, so the same input always yields the same digest.
//! It is not cryptographic — it is only ever used for identity and bucketing, never for
//! security — which is why a tiny in-crate implementation is preferable to pulling a
//! hashing dependency into the leaf of the hexagon.

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The FNV-1a 64-bit digest of `bytes`.
#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A stable 16-hex-digit digest over an ordered list of string `parts`.
///
/// Each part is folded in followed by a `0x1f` unit separator, so `["ab", "c"]` and
/// `["a", "bc"]` hash differently. The result is deterministic across runs and platforms.
#[must_use]
pub fn stable_hash_hex(parts: &[&str]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for part in parts {
        for &byte in part.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash ^= u64::from(0x1f_u8);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// A stable bucket in `0..modulus` for `key`, used for deterministic dataset splitting.
///
/// # Panics
/// Panics if `modulus` is zero.
#[must_use]
pub fn stable_bucket(key: &str, modulus: u64) -> u64 {
    assert!(modulus > 0, "modulus must be non-zero");
    fnv1a64(key.as_bytes()) % modulus
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_the_known_empty_and_single_byte_vectors() {
        // FNV-1a of the empty input is the offset basis; well-known reference vectors.
        assert_eq!(fnv1a64(b""), FNV_OFFSET_BASIS);
        // "a" -> 0xaf63dc4c8601ec8c per the FNV reference test vectors.
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn stable_hash_hex_is_deterministic_and_order_sensitive() {
        assert_eq!(stable_hash_hex(&["x", "y"]), stable_hash_hex(&["x", "y"]));
        assert_ne!(stable_hash_hex(&["x", "y"]), stable_hash_hex(&["y", "x"]));
        // The separator means concatenation ambiguity cannot collide.
        assert_ne!(stable_hash_hex(&["ab", "c"]), stable_hash_hex(&["a", "bc"]));
        assert_eq!(stable_hash_hex(&[]).len(), 16, "always 16 hex digits");
    }

    #[test]
    fn stable_bucket_is_in_range_and_deterministic() {
        for key in ["fb_1", "fb_2", "drffb_999", "clsfb_abc"] {
            let bucket = stable_bucket(key, 100);
            assert!(bucket < 100);
            assert_eq!(bucket, stable_bucket(key, 100), "same key, same bucket");
        }
    }

    #[test]
    #[should_panic(expected = "modulus must be non-zero")]
    fn stable_bucket_rejects_a_zero_modulus() {
        let _ = stable_bucket("k", 0);
    }
}
