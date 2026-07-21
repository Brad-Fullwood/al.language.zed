//! SignalR wire-format types, connection negotiation, and handshake validation
//! for the BC debug hub. Split out of the former monolithic `bc_debug.rs`
//! (pure move, no behavior change).

use serde::Deserialize;
use tracing::warn;

use crate::dap::{DapError, Result};

// `pub(super)` (rather than private): the struct and its fields are
// constructed directly (not just deserialized) by sibling modules —
// `events::signalr_to_bc_event`'s tests, and `session`'s `BcDebugSession`
// fields / `test_new` / `fake` test double — so visibility must extend to
// the whole `bc_debug` module tree, not just this file. Not crate-public.
#[derive(Debug, Deserialize)]
pub(super) struct SignalRMessage {
    #[serde(rename = "type")]
    pub(super) type_: i32,
    // Type 1: invocation (server → client callback)
    pub(super) target: Option<String>,
    pub(super) arguments: Option<Vec<serde_json::Value>>,
    // Type 3: completion (response to our invocation)
    #[serde(rename = "invocationId")]
    pub(super) invocation_id: Option<String>,
    pub(super) result: Option<serde_json::Value>,
    pub(super) error: Option<String>,
    // Type 6: ping
}

/// Per-operation timeout for SignalR `invoke()` calls. Different debug-hub
/// targets have wildly different latency budgets:
///
/// - quick step/continue control flow → a few seconds is plenty
/// - stack/variable inspection → can be slow on deep AL records
/// - attach / disconnect / publish → cover network setup + server-side work
///
/// A blanket 60 s timeout (the prior default) was too short for slow-network
/// attach flows and too long for the user to notice that "Step Over" was
/// silently stuck.
pub(super) fn default_invoke_timeout(target: &str) -> tokio::time::Duration {
    use tokio::time::Duration;
    match target {
        // Step / continue / break — should respond within a couple of seconds
        // on a healthy server. Short timeout so a hung server fails fast.
        "Next" | "StepIn" | "StepOut" | "Continue" | "Break" => Duration::from_secs(10),
        // Connection ping. Should be very fast.
        "IsAlive" => Duration::from_secs(5),
        // Variable inspection / stack frames — can be slow on deep records
        // (BC's GetVariables walks the record graph server-side).
        // `ExpandNode` is the per-row drill-in used by `expand_node`,
        // `GetWatchNode` by `get_watch_node` — both call `invoke()` with
        // those exact target strings, so they belong in this 30s bucket
        // alongside the other variable-walk paths. Previously the table
        // listed `ExpandVariableTree` / `ExpandLocalsTree` (no caller),
        // and the real strings fell through to the 60s catch-all below.
        "GetVariables" | "GetStackTrace" | "ExpandGlobals" | "ExpandNode" | "GetWatchNode"
        | "GetSource" => Duration::from_secs(30),
        // Attach / DebugAdapterConfigurationDone — network setup. Allow a
        // longer budget for high-latency BC SaaS connections.
        "Attach" | "DebugAdapterConfigurationDone" => Duration::from_secs(120),
        // Breakpoint operations — usually fast but can serialize behind a
        // BC compilation step. `UpdateBreakpoint` belongs here too;
        // previously it fell through to the 60s catch-all.
        "AddBreakpoint" | "RemoveBreakpoint" | "UpdateBreakpoint" | "SetBreakpointResponse" => {
            Duration::from_secs(30)
        }
        // Teardown — should be quick; if it isn't, we abandon and tear down
        // the WS connection anyway.
        "StopDebugging" | "TerminateSession" => Duration::from_secs(10),
        // Unknown / future targets — fall back to the previous global value.
        _ => Duration::from_secs(60),
    }
}

/// Percent-encode a string for safe embedding as a URL query parameter value.
///
/// Unreserved characters (RFC 3986) are passed through unchanged; all other
/// bytes are encoded as `%XX`. This is used to sanitize server-returned values
/// (e.g. SignalR `connectionToken`) before they are embedded in WebSocket URLs.
pub(crate) fn percent_encode_url(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len() * 3);
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

/// Outcome of parsing a SignalR `/negotiate?negotiateVersion=1` response.
#[derive(Debug)]
pub(super) struct NegotiateConnection {
    /// The SignalR session id (`connectionId`), surfaced in debug context.
    pub(super) connection_id: String,
    /// The value to pass as the WebSocket `?id=` query parameter. For a
    /// negotiateVersion-1 server this is the `connectionToken`; for a server
    /// that negotiated down to version 0 (no `connectionToken` present) it is
    /// the `connectionId`, per the SignalR transport protocol.
    pub(super) ws_id: String,
}

/// Resolve the WebSocket connection identifiers from a SignalR negotiate
/// response, validating the version the server negotiated.
///
/// The client requests `negotiateVersion=1`. A spec-compliant server echoes
/// the version it actually agreed to via the `negotiateVersion` field:
///   - **1** — the response carries a `connectionToken` (used as the `?id=`
///     value) distinct from `connectionId` (the session id).
///   - **0** — older protocol: no `connectionToken` is returned and
///     `connectionId` doubles as the `?id=` value.
///
/// A missing `negotiateVersion` field is treated as 0 for backward
/// compatibility (pre-versioning SignalR servers). An unrecognised version is
/// logged but handled on a best-effort basis (token if present, else id).
pub(super) fn resolve_negotiate_connection(
    negotiate: &serde_json::Value,
) -> Result<NegotiateConnection> {
    // SignalR redirect responses carry a `url` (and `accessToken`) instead of
    // a connection id. We do not follow redirects, so surface a clear error
    // rather than failing later with a confusing "no connectionId".
    if negotiate.get("url").and_then(|v| v.as_str()).is_some() {
        return Err(DapError::ConnectionFailed(
            "SignalR negotiate returned a redirect response (`url`); redirects are not \
             supported by the BC debug client"
                .to_string(),
        ));
    }

    // The session id is reported as `connectionId`, but BC versions (and the
    // underlying SignalR implementation) have varied the casing, so accept the
    // common variants.
    let connection_id = ["connectionId", "ConnectionId", "connection_id"]
        .iter()
        .find_map(|key| negotiate.get(*key).and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let connection_token = negotiate
        .get("connectionToken")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Absent field defaults to 0 (pre-versioning servers). A non-integer is
    // ignored and treated as absent.
    let negotiated_version = negotiate
        .get("negotiateVersion")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    match negotiated_version {
        1 => {
            // v1 requires both connectionId and connectionToken.
            let connection_id = connection_id.ok_or_else(|| {
                DapError::ConnectionFailed(
                    "SignalR negotiateVersion=1 response has no connectionId field (checked \
                     connectionId/ConnectionId/connection_id)"
                        .to_string(),
                )
            })?;
            let connection_token = connection_token.ok_or_else(|| {
                DapError::ConnectionFailed(
                    "SignalR negotiateVersion=1 response has no connectionToken".to_string(),
                )
            })?;
            Ok(NegotiateConnection {
                ws_id: connection_token,
                connection_id,
            })
        }
        0 => {
            // v0: no connectionToken; connectionId is the session id AND the
            // ?id= value.
            let connection_id = connection_id.ok_or_else(|| {
                DapError::ConnectionFailed(
                    "SignalR negotiateVersion=0 response has no connectionId field (checked \
                     connectionId/ConnectionId/connection_id)"
                        .to_string(),
                )
            })?;
            Ok(NegotiateConnection {
                ws_id: connection_id.clone(),
                connection_id,
            })
        }
        other => {
            // Unexpected version the BC server claims to speak. Don't hard-fail
            // — handle best-effort (token if present, else id) and warn so the
            // mismatch is visible if the debug session misbehaves.
            warn!(
                "SignalR negotiate returned unexpected negotiateVersion={other} (client \
                 requested 1); proceeding best-effort"
            );
            let connection_id = connection_id.ok_or_else(|| {
                DapError::ConnectionFailed(format!(
                    "SignalR negotiateVersion={other} response has no connectionId field"
                ))
            })?;
            let ws_id = connection_token.unwrap_or_else(|| connection_id.clone());
            Ok(NegotiateConnection {
                ws_id,
                connection_id,
            })
        }
    }
}

/// Replace the value of `connectionToken` (SignalR session credential) in a
/// JSON response body with a `<redacted>` placeholder before logging or
/// surfacing in errors. Falls back to the original text if the body is not
/// valid JSON or has no such field.
/// Validate the SignalR handshake response.
///
/// After the client sends `{"protocol":"json","version":1}`, a spec-compliant
/// SignalR server replies with one of:
/// - `{}` — handshake accepted; the client may proceed.
/// - `{"error":"<reason>"}` — handshake **rejected** (e.g. the server does not
///   support the requested protocol/version). The connection is unusable.
///
/// Older code only `debug!`-logged this frame, so a protocol/version mismatch
/// fell through silently and the session limped on against an adapter that
/// would misbehave on every later invoke. This surfaces the rejection loudly
/// as a `ConnectionFailed`, which is exactly the "version-detection /
/// capability probe should fail loudly" guarantee.
///
/// A frame that does not parse as JSON, or that parses but carries no `error`
/// field, is treated as accepted (`Ok(())`): some servers send the success
/// frame coalesced with the first real message, and we must not regress a
/// working handshake. The strict signal we act on is solely a present,
/// non-empty `error`.
pub(super) fn validate_signalr_handshake_response(frame: &str) -> Result<()> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        // Not JSON we can interpret — don't fail a possibly-fine handshake.
        return Ok(());
    };
    if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
        if !err.is_empty() {
            return Err(DapError::ConnectionFailed(format!(
                "SignalR handshake rejected by BC server: {err}. The debug \
                 protocol/version the client offered (json v1) was refused — \
                 the server may speak an incompatible SignalR protocol version."
            )));
        }
    }
    Ok(())
}

pub(super) fn redact_connection_token(body: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    if let Some(map) = v.as_object_mut() {
        for key in ["connectionToken", "ConnectionToken", "accessToken"] {
            if map.contains_key(key) {
                map.insert(
                    key.to_string(),
                    serde_json::Value::String("<redacted>".to_string()),
                );
            }
        }
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_timeout_step_ops_are_short() {
        // Positive: step / continue / break should respond within seconds;
        // they get a short timeout so a hung server fails fast and the user
        // notices instead of waiting a full minute.
        for target in ["Next", "StepIn", "StepOut", "Continue", "Break"] {
            let t = default_invoke_timeout(target);
            assert!(
                t <= tokio::time::Duration::from_secs(15),
                "{target} timeout {t:?} should be ≤ 15s"
            );
        }
    }

    #[test]
    fn invoke_timeout_variable_inspection_is_medium() {
        // Positive: variable / stack-frame inspection can be slow on deep
        // BC records but shouldn't take more than ~30s either.
        for target in ["GetVariables", "GetStackTrace", "ExpandGlobals"] {
            let t = default_invoke_timeout(target);
            assert!(
                t >= tokio::time::Duration::from_secs(15)
                    && t <= tokio::time::Duration::from_secs(60),
                "{target} timeout {t:?} should be in [15s, 60s]"
            );
        }
    }

    #[test]
    fn invoke_timeout_attach_is_generous() {
        // Positive: attach / DebugAdapterConfigurationDone include network
        // setup against potentially-slow BC SaaS endpoints; need budget.
        for target in ["Attach", "DebugAdapterConfigurationDone"] {
            let t = default_invoke_timeout(target);
            assert!(
                t >= tokio::time::Duration::from_secs(60),
                "{target} timeout {t:?} should be ≥ 60s for SaaS latency headroom"
            );
        }
    }

    #[test]
    fn invoke_timeout_unknown_target_falls_back_to_60s() {
        // Negative: an unrecognised target (future BC protocol additions, or
        // a typo in our code) must still produce a finite, reasonable
        // default rather than panic or return zero.
        let t = default_invoke_timeout("SomeFutureUnknownTarget");
        assert_eq!(t, tokio::time::Duration::from_secs(60));
    }

    #[test]
    fn invoke_timeout_expand_node_uses_variable_bucket() {
        // `expand_node` calls invoke("ExpandNode"),
        // `get_watch_node` calls invoke("GetWatchNode"). Previously the
        // table listed `ExpandVariableTree` / `ExpandLocalsTree` (no real
        // caller); the actual strings fell through to the 60s catch-all.
        // Both must now hit the 30s variable-inspection bucket.
        assert_eq!(
            default_invoke_timeout("ExpandNode"),
            tokio::time::Duration::from_secs(30)
        );
        assert_eq!(
            default_invoke_timeout("GetWatchNode"),
            tokio::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn invoke_timeout_update_breakpoint_uses_breakpoint_bucket() {
        // `UpdateBreakpoint` is a real invoke target
        // (alongside AddBreakpoint / RemoveBreakpoint / SetBreakpointResponse)
        // and must use the 30s breakpoint-operations budget rather than the
        // 60s unknown-target fallback.
        assert_eq!(
            default_invoke_timeout("UpdateBreakpoint"),
            tokio::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn invoke_timeout_dead_entries_are_gone() {
        // The old table had entries for
        // `ExpandVariableTree` and `ExpandLocalsTree` that no caller ever
        // produced. Those strings should now fall through to the 60s
        // catch-all (since they are unreachable by the codebase). This
        // test pins the cleanup so a future revert doesn't silently
        // reintroduce dead table entries.
        assert_eq!(
            default_invoke_timeout("ExpandVariableTree"),
            tokio::time::Duration::from_secs(60)
        );
        assert_eq!(
            default_invoke_timeout("ExpandLocalsTree"),
            tokio::time::Duration::from_secs(60)
        );
    }

    #[test]
    fn redact_connection_token_replaces_field() {
        let body = r#"{"connectionToken":"secret-abc","url":"/signalr"}"#;
        let redacted = redact_connection_token(body);
        assert!(!redacted.contains("secret-abc"));
        assert!(redacted.contains("<redacted>"));
        assert!(redacted.contains("/signalr"));
    }

    #[test]
    fn redact_connection_token_passthrough_on_invalid_json() {
        let body = "not-json garbage";
        assert_eq!(redact_connection_token(body), "not-json garbage");
    }

    #[test]
    fn handshake_empty_frame_is_accepted() {
        // The canonical SignalR success response is `{}`; some servers also
        // send a coalesced/empty frame. Neither is a rejection.
        assert!(validate_signalr_handshake_response("").is_ok());
        assert!(validate_signalr_handshake_response("   ").is_ok());
        assert!(validate_signalr_handshake_response("{}").is_ok());
    }

    #[test]
    fn handshake_non_json_frame_is_accepted() {
        // A frame we cannot parse must not regress an otherwise-working
        // handshake; only an explicit `error` field is treated as a rejection.
        assert!(validate_signalr_handshake_response("not json at all").is_ok());
    }

    #[test]
    fn handshake_empty_error_field_is_accepted() {
        // An empty `error` string is not a real rejection signal.
        assert!(validate_signalr_handshake_response(r#"{"error":""}"#).is_ok());
    }

    #[test]
    fn handshake_error_field_fails_loudly() {
        // The protocol/version-mismatch signal: a non-empty `error` must
        // surface as a ConnectionFailed carrying the server's reason, instead
        // of falling through silently.
        let res = validate_signalr_handshake_response(
            r#"{"error":"Requested protocol version is not supported"}"#,
        );
        let err = res.expect_err("non-empty handshake error must fail");
        assert!(
            matches!(&err, DapError::ConnectionFailed(m)
                if m.contains("handshake rejected")
                    && m.contains("Requested protocol version is not supported")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn handshake_no_error_field_with_other_keys_is_accepted() {
        // A success frame may carry additional non-error keys; still accepted.
        assert!(validate_signalr_handshake_response(r#"{"minorVersion":2}"#).is_ok());
    }

    #[test]
    fn redact_connection_token_no_field_unchanged_logically() {
        let body = r#"{"foo":"bar"}"#;
        let out = redact_connection_token(body);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["foo"], "bar");
        assert!(parsed.get("connectionToken").is_none());
    }

    #[test]
    fn negotiate_v1_uses_token_as_ws_id_and_id_as_session() {
        // negotiateVersion=1: the WebSocket ?id= must be the connectionToken,
        // while the surfaced session id is the connectionId — they differ.
        let v = serde_json::json!({
            "negotiateVersion": 1,
            "connectionId": "session-abc",
            "connectionToken": "token-xyz",
        });
        let r = resolve_negotiate_connection(&v).unwrap();
        assert_eq!(r.ws_id, "token-xyz");
        assert_eq!(r.connection_id, "session-abc");
    }

    #[test]
    fn negotiate_v0_uses_connection_id_for_both() {
        // negotiateVersion=0: no connectionToken is returned; connectionId is
        // both the session id and the ?id= value. Old code hard-failed here
        // with "No connectionToken in negotiate".
        let v = serde_json::json!({
            "negotiateVersion": 0,
            "connectionId": "session-only",
        });
        let r = resolve_negotiate_connection(&v).unwrap();
        assert_eq!(r.ws_id, "session-only");
        assert_eq!(r.connection_id, "session-only");
    }

    #[test]
    fn negotiate_missing_version_defaults_to_v0() {
        // A pre-versioning server omits negotiateVersion entirely; treat as v0
        // and use connectionId for the WebSocket ?id=.
        let v = serde_json::json!({
            "connectionId": "legacy-session",
        });
        let r = resolve_negotiate_connection(&v).unwrap();
        assert_eq!(r.ws_id, "legacy-session");
        assert_eq!(r.connection_id, "legacy-session");
    }

    #[test]
    fn negotiate_v1_missing_token_errors() {
        // negotiateVersion=1 without a connectionToken is malformed and must
        // surface a clear error rather than silently substituting the id.
        let v = serde_json::json!({
            "negotiateVersion": 1,
            "connectionId": "session-abc",
        });
        let err = resolve_negotiate_connection(&v).unwrap_err();
        assert!(
            matches!(&err, DapError::ConnectionFailed(m) if m.contains("connectionToken")),
            "expected connectionToken error, got {err:?}"
        );
    }

    #[test]
    fn negotiate_missing_connection_id_errors() {
        // No connectionId in any recognised casing must error, not panic or
        // produce an empty ?id=.
        let v = serde_json::json!({
            "negotiateVersion": 0,
            "somethingElse": "value",
        });
        let err = resolve_negotiate_connection(&v).unwrap_err();
        assert!(
            matches!(&err, DapError::ConnectionFailed(m) if m.contains("connectionId")),
            "expected connectionId error, got {err:?}"
        );
    }

    #[test]
    fn negotiate_accepts_connection_id_casing_variants() {
        // BC has shipped PascalCase ConnectionId; accept it under v0.
        let v = serde_json::json!({
            "negotiateVersion": 0,
            "ConnectionId": "pascal-session",
        });
        let r = resolve_negotiate_connection(&v).unwrap();
        assert_eq!(r.connection_id, "pascal-session");
        assert_eq!(r.ws_id, "pascal-session");
    }

    #[test]
    fn negotiate_redirect_response_errors() {
        // SignalR redirect responses carry `url`; we don't follow them, so
        // surface a clear error instead of a confusing missing-id failure.
        let v = serde_json::json!({
            "url": "https://other.example/hub",
            "accessToken": "redir-token",
        });
        let err = resolve_negotiate_connection(&v).unwrap_err();
        assert!(
            matches!(&err, DapError::ConnectionFailed(m) if m.contains("redirect")),
            "expected redirect error, got {err:?}"
        );
    }

    #[test]
    fn negotiate_unexpected_version_best_effort_uses_token() {
        // An unexpected negotiateVersion (e.g. a future 2) must not hard-fail;
        // prefer the token when present.
        let v = serde_json::json!({
            "negotiateVersion": 2,
            "connectionId": "session-abc",
            "connectionToken": "token-xyz",
        });
        let r = resolve_negotiate_connection(&v).unwrap();
        assert_eq!(r.ws_id, "token-xyz");
        assert_eq!(r.connection_id, "session-abc");
    }

    #[test]
    fn percent_encode_url_safe_chars_unchanged() {
        assert_eq!(percent_encode_url("abc-123_XYZ.~"), "abc-123_XYZ.~");
    }

    #[test]
    fn percent_encode_url_encodes_special_chars() {
        let token = "token=value&other=x";
        let encoded = percent_encode_url(token);
        assert!(!encoded.contains('='), "= should be encoded: {encoded}");
        assert!(!encoded.contains('&'), "& should be encoded: {encoded}");
        assert!(encoded.contains("%3D"), "= → %3D: {encoded}");
        assert!(encoded.contains("%26"), "& → %26: {encoded}");
    }

    #[test]
    fn percent_encode_url_empty_string() {
        assert_eq!(percent_encode_url(""), "");
    }

    #[test]
    fn invoke_timeout_isalive_is_5s() {
        assert_eq!(
            default_invoke_timeout("IsAlive"),
            tokio::time::Duration::from_secs(5)
        );
    }

    #[test]
    fn invoke_timeout_teardown_is_10s() {
        for target in ["StopDebugging", "TerminateSession"] {
            assert_eq!(
                default_invoke_timeout(target),
                tokio::time::Duration::from_secs(10),
                "{target} should get the 10s teardown budget"
            );
        }
    }

    #[test]
    fn invoke_timeout_breakpoint_ops_are_30s() {
        // Breakpoint operations can serialise behind a BC compilation step.
        for target in [
            "AddBreakpoint",
            "RemoveBreakpoint",
            "UpdateBreakpoint",
            "SetBreakpointResponse",
        ] {
            assert_eq!(
                default_invoke_timeout(target),
                tokio::time::Duration::from_secs(30),
                "{target} should get the 30s breakpoint budget"
            );
        }
    }

    #[test]
    fn invoke_timeout_get_source_is_variable_bucket() {
        assert_eq!(
            default_invoke_timeout("GetSource"),
            tokio::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn redact_connection_token_redacts_pascal_case_and_access_token() {
        // The redactor must scrub every credential-bearing key variant, not
        // just the lowercase connectionToken.
        let body = r#"{"ConnectionToken":"ct-secret","accessToken":"at-secret","url":"/hub"}"#;
        let out = redact_connection_token(body);
        assert!(
            !out.contains("ct-secret"),
            "ConnectionToken not redacted: {out}"
        );
        assert!(
            !out.contains("at-secret"),
            "accessToken not redacted: {out}"
        );
        assert!(out.contains("/hub"), "non-secret fields preserved: {out}");
    }
}
