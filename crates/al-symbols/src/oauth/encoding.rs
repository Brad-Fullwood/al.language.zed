//! Percent, base64url and HTML encoding used on the wire and in the
//! browser-facing response pages.

/// Base64url encoding without padding (RFC 7636).
pub(super) fn base64url_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3F) as usize] as char);
        }
    }
    out
}

/// Escape HTML special characters to prevent XSS in the OAuth redirect page.
///
/// The OAuth redirect page renders server-returned values (`error`, `error_description`)
/// directly in HTML. These values come from the authorization server redirect URL and
/// could contain `<script>` or other HTML if the user was redirected to a malicious server.
pub(super) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

pub(super) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}

pub(super) fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        // Also decode '+' as space (form encoding)
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn base64url_known_value() {
        // SHA256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = Sha256::digest(b"");
        let encoded = base64url_encode(&hash);
        assert_eq!(encoded, "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");
    }

    #[test]
    fn percent_encode_roundtrip() {
        let input = "https://api.businesscentral.dynamics.com/.default offline_access";
        let encoded = percent_encode(input);
        assert!(encoded.contains("%3A"));
        assert!(encoded.contains("%20"));
        let decoded = percent_decode(&encoded);
        assert_eq!(decoded, input);
    }

    #[test]
    fn html_escape_neutralizes_script_injection() {
        // The redirect page renders server-supplied error/description verbatim;
        // a malicious authorization server could inject markup. Every HTML
        // metacharacter must be entity-encoded.
        let out = html_escape("<script>alert('x&y')</script>\"q\"");
        assert!(!out.contains('<'), "raw '<' must not survive: {out}");
        assert!(!out.contains('>'), "raw '>' must not survive: {out}");
        assert_eq!(
            out,
            "&lt;script&gt;alert(&#x27;x&amp;y&#x27;)&lt;/script&gt;&quot;q&quot;"
        );
    }

    #[test]
    fn html_escape_leaves_safe_text_untouched() {
        // Boundary: ordinary text (incl. non-ASCII) passes through verbatim.
        let s = "Signed in: café 123";
        assert_eq!(html_escape(s), s);
    }

    #[test]
    fn percent_decode_handles_truncated_and_invalid_escapes() {
        // A trailing '%' with no following hex digits must be preserved, not
        // panic or eat past the end of the string.
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("a%2"), "a%2");
        // A '%' followed by non-hex is left as a literal '%'.
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("a%2Bb+c"), "a+b c");
    }
}
