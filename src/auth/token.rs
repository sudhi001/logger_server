//! Token generation and hashing.
//!
//! Tokens are 32 characters of OS randomness in an unambiguous alphabet. Only
//! their SHA-256 digest is stored, so a database leak does not hand out working
//! credentials. A plain digest is correct here: unlike a password, a token has
//! full entropy, so there is nothing for a slow KDF to protect against.

use sha2::{Digest, Sha256};

/// Digits and letters minus the ones people misread (0/O, 1/l/I).
const ALPHABET: &[u8] = b"23456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";
const TOKEN_BODY_LEN: usize = 32;

pub const DEVICE_PREFIX: &str = "lgrd_";
pub const ADMIN_PREFIX: &str = "lgra_";

pub type TokenHash = [u8; 32];

/// Generates a new token with the given prefix.
pub fn generate(prefix: &str) -> String {
    let mut raw = [0u8; TOKEN_BODY_LEN];
    getrandom::fill(&mut raw).expect("OS randomness unavailable");

    let mut out = String::with_capacity(prefix.len() + TOKEN_BODY_LEN);
    out.push_str(prefix);
    for byte in raw {
        // Modulo bias over a 56-character alphabet is negligible against the
        // 180+ bits of entropy this carries.
        out.push(ALPHABET[byte as usize % ALPHABET.len()] as char);
    }
    out
}

pub fn hash(token: &str) -> TokenHash {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

/// Leading characters shown in the UI so a token is recognisable without being
/// revealed. The secret body is never displayed again after creation.
pub fn display_prefix(token: &str) -> String {
    token.chars().take(12).collect()
}

/// Compares in time independent of how many leading bytes match, so a caller
/// cannot use response timing to recover the value byte by byte.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extracts a bearer token from `Authorization`, falling back to an explicit
/// header that some mobile HTTP clients find easier to set.
pub fn from_headers<'a>(headers: &'a axum::http::HeaderMap, explicit: &str) -> Option<&'a str> {
    if let Some(v) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    headers
        .get(explicit)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}
