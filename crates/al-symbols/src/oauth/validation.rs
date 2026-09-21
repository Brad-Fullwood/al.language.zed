//! Shape checks on caller-supplied tenant and client identifiers, applied
//! before anything reaches the network.

/// Validate a tenant identifier before interpolating it into Microsoft's
/// OAuth URLs. Accepts:
///   - A well-formed GUID (e.g. `12345678-1234-1234-1234-123456789012`)
///   - The well-known reserved names `common`, `organizations`, `consumers`
///   - A domain like `contoso.onmicrosoft.com` (letters/digits/`.`/`-`/`_`)
///
/// Rejects anything containing `/`, `?`, `#`, whitespace, or other URL
/// punctuation that could redirect / poison the request.
pub(super) fn is_valid_tenant(s: &str) -> bool {
    if s.is_empty() || s.len() > 256 {
        return false;
    }
    if matches!(s, "common" | "organizations" | "consumers") {
        return true;
    }
    if is_well_formed_guid(s) {
        return true;
    }
    // Domain-shaped: alphanumeric segments separated by dots, optional
    // hyphens / underscores. No `/`, `?`, `#`, ':', '@', whitespace.
    let domain_chars = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_';
    s.chars().all(domain_chars) && s.contains('.')
}

/// True iff `s` is a 36-character hyphenated GUID
/// (8-4-4-4-12, hex elsewhere). Used by acquire_token to validate
/// `BC_CLIENT_ID` before interpolating it into the authorization URL.
pub(super) fn is_well_formed_guid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        let is_hyphen_pos = matches!(i, 8 | 13 | 18 | 23);
        if is_hyphen_pos {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod guid_tests {
    use super::is_well_formed_guid;
    use crate::oauth::flows::{configured_client_id, DEFAULT_CLIENT_ID};
    use serial_test::serial;
    #[test]
    fn accepts_canonical_aad_app_id() {
        assert!(is_well_formed_guid("ef72a0a7-b59c-4f97-99c8-5b9a2cd3a1b6"));
        // Real BC default client id (uppercase hex)
        assert!(is_well_formed_guid("ABCDEF12-3456-7890-ABCD-EF1234567890"));
    }
    #[test]
    fn rejects_obvious_bad_shapes() {
        assert!(!is_well_formed_guid(""));
        assert!(!is_well_formed_guid("not-a-guid"));
        // 35 chars
        assert!(!is_well_formed_guid("ef72a0a7-b59c-4f97-99c8-5b9a2cd3a1b"));
        // 37 chars
        assert!(!is_well_formed_guid(
            "ef72a0a7-b59c-4f97-99c8-5b9a2cd3a1b66"
        ));
        // wrong hyphen position
        assert!(!is_well_formed_guid(
            "ef72a0a-7b59c-4f97-99c8-5b9a2cd3a1b66"
        ));
        // non-hex
        assert!(!is_well_formed_guid("zf72a0a7-b59c-4f97-99c8-5b9a2cd3a1b6"));
    }

    #[test]
    #[serial]
    fn missing_client_id_uses_default() {
        let previous = std::env::var_os("BC_CLIENT_ID");
        std::env::remove_var("BC_CLIENT_ID");
        assert_eq!(configured_client_id().unwrap(), DEFAULT_CLIENT_ID);
        if let Some(value) = previous {
            std::env::set_var("BC_CLIENT_ID", value);
        }
    }

    #[test]
    #[serial]
    fn invalid_client_id_is_rejected() {
        let previous = std::env::var_os("BC_CLIENT_ID");
        std::env::set_var("BC_CLIENT_ID", "not-a-guid");
        assert!(configured_client_id().is_err());
        match previous {
            Some(value) => std::env::set_var("BC_CLIENT_ID", value),
            None => std::env::remove_var("BC_CLIENT_ID"),
        }
    }
}

#[cfg(test)]
mod tenant_tests {
    use super::is_valid_tenant;

    #[test]
    fn accepts_guid_tenant() {
        assert!(is_valid_tenant("12345678-1234-1234-1234-123456789012"));
    }

    #[test]
    fn accepts_reserved_names() {
        assert!(is_valid_tenant("common"));
        assert!(is_valid_tenant("organizations"));
        assert!(is_valid_tenant("consumers"));
    }

    #[test]
    fn accepts_domain_tenants() {
        assert!(is_valid_tenant("contoso.onmicrosoft.com"));
        assert!(is_valid_tenant("my-org.example.com"));
        assert!(is_valid_tenant("a.b.c.d"));
    }

    #[test]
    fn rejects_empty_and_oversize() {
        assert!(!is_valid_tenant(""));
        let long = "a".repeat(257);
        assert!(!is_valid_tenant(&long));
    }

    #[test]
    fn rejects_url_punctuation() {
        // Negative: anything that could redirect or poison the URL must be rejected.
        assert!(!is_valid_tenant("contoso.com/extra"));
        assert!(!is_valid_tenant("contoso.com?query=evil"));
        assert!(!is_valid_tenant("contoso.com#frag"));
        assert!(!is_valid_tenant("evil@contoso.com"));
        assert!(!is_valid_tenant("contoso .com")); // whitespace
        assert!(!is_valid_tenant("contoso\ncom")); // newline
        assert!(!is_valid_tenant("http://contoso.com"));
        assert!(!is_valid_tenant("///pwned"));
    }

    #[test]
    fn rejects_bare_word_without_dot() {
        // Looks like it could be a single-segment domain but isn't one of the
        // reserved names — reject so a typo of "common" doesn't sneak through.
        assert!(!is_valid_tenant("randomword"));
    }
}
