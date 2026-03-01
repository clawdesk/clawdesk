//! Shared cryptographic primitives — SHA-256, HMAC-SHA256, constant-time comparison.
//!
//! Single implementation used by both `clawdesk-gateway` (bearer token auth)
//! and `clawdesk-security` (scoped token signing). Eliminates the previous
//! code duplication where two independent SHA-256 implementations could
//! diverge and cause subtle verification failures.
//!
//! # Security Properties
//!
//! - `sha256`: Delegates to the audited `sha2` crate (RustCrypto).
//! - `hmac_sha256`: Delegates to the audited `hmac` crate (RustCrypto).
//! - `constant_time_eq`: Delegates to `subtle::ConstantTimeEq` — formally
//!   verified constant-time on all target architectures.

use hmac::{Hmac, Mac};
use sha2::{Sha256, Digest};
use subtle::ConstantTimeEq;

/// SHA-256 hash. Delegates to the `sha2` crate (RustCrypto).
///
/// Constant-time with respect to data on all architectures (handled by
/// the `sha2` crate's platform-specific backends including hardware
/// SHA extensions on x86 and ARMv8).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// HMAC-SHA256 (RFC 2104) for scoped token signing.
///
/// Delegates to the `hmac` crate (RustCrypto). Zero heap allocation.
pub fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Constant-time byte comparison. Always examines all 32 bytes regardless
/// of where they differ, preventing timing side-channel attacks.
///
/// Delegates to `subtle::ConstantTimeEq` — formally verified to be
/// constant-time on all target architectures via compiler barriers.
#[inline(never)]
pub fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = sha256(b"");
        assert_eq!(
            hash,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
                0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
                0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
                0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
            ]
        );
    }

    #[test]
    fn sha256_abc_vector() {
        let hash = sha256(b"abc");
        assert_eq!(
            hash,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea,
                0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
                0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c,
                0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn hmac_sha256_known_vector() {
        let mut key = [0u8; 32];
        for i in 0..20 {
            key[i] = 0x0b;
        }
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            mac,
            [
                0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53,
                0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1, 0x2b,
                0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7,
                0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32, 0xcf, 0xf7,
            ]
        );
    }

    #[test]
    fn constant_time_eq_same() {
        let a = sha256(b"test");
        let b = sha256(b"test");
        assert!(constant_time_eq(&a, &b));
    }

    #[test]
    fn constant_time_eq_different() {
        let a = sha256(b"a");
        let b = sha256(b"b");
        assert!(!constant_time_eq(&a, &b));
    }

    #[test]
    fn constant_time_eq_single_bit() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        b[31] = 1;
        assert!(!constant_time_eq(&a, &b));
        a[31] = 1;
        assert!(constant_time_eq(&a, &b));
    }
}
