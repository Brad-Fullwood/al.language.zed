//! PKCE verifier and challenge generation.

use super::encoding::base64url_encode;
use super::OAuthError;
use sha2::{Digest, Sha256};

pub(super) fn generate_code_verifier() -> Result<String, OAuthError> {
    let bytes = random_bytes(32)?;
    Ok(base64url_encode(&bytes))
}

pub(super) fn pkce_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64url_encode(&hash)
}

pub(super) fn generate_random_string(len: usize) -> Result<String, OAuthError> {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    // Rejection sampling threshold: largest multiple of CHARS.len() (62) that
    // fits in a u8. 256 - (256 % 62) = 248. Bytes >= 248 would skew the
    // distribution toward chars 0..7 if folded with %, so we discard them
    // and draw fresh bytes.  Worst case ratio is 248/256 ≈ 96.875% accept,
    // so the loop terminates in expected O(len) draws.
    const ACCEPT_LT: u8 = (u8::MAX as usize - (u8::MAX as usize % CHARS.len())) as u8;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let need = len - out.len();
        // Draw a buffer larger than `need` to amortise the syscall cost when
        // ~3% of bytes will be rejected.
        let bytes = random_bytes(need + need / 16 + 1)?;
        for b in bytes {
            if b < ACCEPT_LT {
                out.push(CHARS[(b as usize) % CHARS.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Generate `n` cryptographically random bytes using the OS entropy source.
/// Uses `getrandom` which works on Linux, macOS, Windows, and WASM.
fn random_bytes(n: usize) -> Result<Vec<u8>, OAuthError> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| OAuthError::Protocol {
        error: "getrandom_failed".to_string(),
        description: format!("Failed to get random bytes: {e}"),
    })?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_and_challenge() {
        let verifier = generate_code_verifier().expect("random bytes available in test");
        assert!(verifier.len() >= 43); // 32 bytes → 43 base64url chars
        let challenge = pkce_challenge(&verifier);
        assert!(challenge.len() >= 43);
        // Challenge should differ from verifier (it's a hash)
        assert_ne!(verifier, challenge);
    }

    #[test]
    fn generate_random_string_respects_length_and_charset() {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for len in [0usize, 1, 16, 100] {
            let s = generate_random_string(len).expect("entropy available in test");
            assert_eq!(s.len(), len, "exact requested length");
            assert!(
                s.bytes().all(|b| CHARS.contains(&b)),
                "only the URL-safe alphabet may appear: {s:?}"
            );
        }
    }
}
