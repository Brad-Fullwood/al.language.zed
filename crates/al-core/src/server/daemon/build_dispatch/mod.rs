//! Build/toolchain/analysis dispatchers — compile, package, lint, format, fix, permissions,
//! authenticate, download symbols, snapshot, profiling, xliff, etc.

mod build;
mod codegen;
mod fixes;
mod symbols_auth;
mod tests_dispatch;
mod xliff;

// Re-export everything from submodules so daemon/mod.rs call sites are unchanged.
pub(super) use build::*;
pub(super) use codegen::*;
pub(super) use fixes::*;
pub(super) use symbols_auth::*;
pub(super) use tests_dispatch::*;
pub(super) use xliff::*;

use crate::workspace::Workspace;
use al_protocol::jsonrpc::Response;

// Shared constants — used by multiple submodules via `super::ERR_*`
pub(super) const ERR_INITIALIZING: &str = "Workspace is initializing, try again";
pub(super) const ERR_NO_PROJECT: &str = "No project loaded";

/// Largest realistic AL procedure body is ~5k tokens; cap the
/// `minTokens` duplicate-detection threshold at 10k so a hostile or
/// fat-fingered client can't (a) push the threshold above any real
/// procedure (effectively disabling detection) or (b) drive the
/// scan loop into pathological territory. F-OPEN-007.
const MAX_DUPLICATES_MIN_TOKENS: u64 = 10_000;

/// Clamp the duplicate-detection `minTokens` param to a sensible upper
/// bound; default 20 when absent.
fn clamp_min_tokens(t: Option<u64>) -> usize {
    t.unwrap_or(20).min(MAX_DUPLICATES_MIN_TOKENS) as usize
}

/// Clamp the duplicate-detection `minSimilarity` ratio to `[0.0, 1.0]`.
/// NaN / ±inf fall back to the default (0.8) so a hostile or garbage
/// value can't disable the filter or cause downstream comparison
/// surprises. F-OPEN-007.
fn clamp_min_similarity(s: Option<f64>) -> f32 {
    let raw = s.unwrap_or(0.8);
    if raw.is_finite() {
        raw.clamp(0.0, 1.0) as f32
    } else {
        0.8
    }
}

// Analysis dispatchers (no dedicated submodule — reported as left-in-mod)

pub(super) fn dispatch_obsolete(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::obsolescence::obsolescence_timeline(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_audit_data_classification(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::audit::data_classification_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_permission_set_audit(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::audit::permission_set_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_deps_graph(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    // Read app.json from project root
    let app_json = workspace
        .project
        .try_read()
        .ok()
        .and_then(|p| p.as_ref().map(|p| p.root.join("app.json")))
        .and_then(|path| {
            tokio::task::block_in_place(|| std::fs::read_to_string(&path))
                .map_err(|e| {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read app.json — proceeding with empty manifest");
                    e
                })
                .ok()
        })
        .unwrap_or_default();

    // Build package list from loaded symbols — name, publisher, version, deps.
    // Currently we pass the packages list without transitive dependency info;
    // the dep graph will still resolve direct dependencies from app.json.
    // Uses the `PackageEntry` alias defined in `queries::deps` so the type
    // stays in one place if its shape ever changes.
    let packages: Vec<crate::queries::deps::PackageEntry> = Vec::new();

    let graph = crate::queries::deps::build_dependency_graph(&app_json, &packages);

    if format == "dot" {
        let dot = graph.to_dot();
        Response {
            id,
            result: Some(serde_json::json!({ "format": "dot", "content": dot })),
            error: None,
            ..Default::default()
        }
    } else {
        let value = serde_json::to_value(&graph).unwrap_or(serde_json::Value::Null);
        Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        }
    }
}

pub(super) fn dispatch_breaking_changes(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    // Compare baseline (empty) against current workspace symbols to find
    // all changes relative to a clean slate.  Callers can pass baseline
    // symbols in params.baselineSymbols in a future iteration.
    let current: Vec<crate::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline: Vec<crate::symbols::SymbolEntry> = Vec::new();
    let changes = crate::queries::breaking_changes::analyze_breaking_changes(&baseline, &current);
    let value = serde_json::to_value(&changes).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_find_duplicates(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // F-OPEN-007: bound user-supplied numeric params at the daemon boundary.
    let min_tokens = clamp_min_tokens(params.get("minTokens").and_then(|v| v.as_u64()));
    let min_similarity = clamp_min_similarity(params.get("minSimilarity").and_then(|v| v.as_f64()));
    let duplicates =
        crate::queries::duplicates::find_duplicates(workspace, min_tokens, min_similarity);
    let value = serde_json::to_value(&duplicates).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_upgrade_report(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    // Use empty baseline to find all symbols that are new/changed relative
    // to a fresh install.  In practice callers supply a previous .app snapshot.
    let current: Vec<crate::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline: Vec<crate::symbols::SymbolEntry> = Vec::new();
    let issues = crate::queries::upgrade::upgrade_report(&baseline, &current);
    let value = serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_sql_patterns(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    let findings = crate::queries::sql_patterns::detect_sql_patterns(workspace);
    let value = serde_json::to_value(&findings).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::workspace::Workspace;
    pub(crate) fn empty_ws() -> Workspace {
        Workspace::new()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::empty_ws;
    use super::*;
    use crate::workspace::Workspace;
    use al_protocol::jsonrpc::error_codes;

    // --- clamp_min_tokens / clamp_min_similarity (F-OPEN-007) ----------------

    #[test]
    fn clamp_min_tokens_defaults_when_absent() {
        // None → the documented default of 20.
        assert_eq!(clamp_min_tokens(None), 20);
    }

    #[test]
    fn clamp_min_tokens_passes_through_sensible_values() {
        assert_eq!(clamp_min_tokens(Some(0)), 0);
        assert_eq!(clamp_min_tokens(Some(50)), 50);
        assert_eq!(
            clamp_min_tokens(Some(MAX_DUPLICATES_MIN_TOKENS)),
            MAX_DUPLICATES_MIN_TOKENS as usize
        );
    }

    #[test]
    fn clamp_min_tokens_caps_oversized_input() {
        // Hostile / fat-fingered values cap at MAX, not panic and not pass through.
        assert_eq!(
            clamp_min_tokens(Some(u64::MAX)),
            MAX_DUPLICATES_MIN_TOKENS as usize
        );
        assert_eq!(
            clamp_min_tokens(Some(MAX_DUPLICATES_MIN_TOKENS + 1)),
            MAX_DUPLICATES_MIN_TOKENS as usize
        );
    }

    #[test]
    fn clamp_min_similarity_defaults_when_absent() {
        // None → the documented default of 0.8.
        assert!((clamp_min_similarity(None) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_clamps_in_range() {
        assert!((clamp_min_similarity(Some(0.0)) - 0.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(0.5)) - 0.5).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(1.0)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_rejects_out_of_range() {
        // Negative reals clamp to 0, super-1 to 1.
        assert!((clamp_min_similarity(Some(-1.0)) - 0.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(2.5)) - 1.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(1e308)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_rejects_non_finite() {
        // NaN / ±inf must fall back to the safe default, not propagate and
        // poison downstream `>=` comparisons.
        assert!((clamp_min_similarity(Some(f64::NAN)) - 0.8).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(f64::INFINITY)) - 0.8).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(f64::NEG_INFINITY)) - 0.8).abs() < 1e-6);
    }

    // --- dispatch_deps_graph -------------------------------------------------

    #[test]
    fn deps_graph_dot_format_returns_dot_content() {
        let ws = empty_ws();
        let resp = dispatch_deps_graph(&ws, 1, &serde_json::json!({ "format": "dot" }));
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert_eq!(r["format"], serde_json::json!("dot"));
        assert!(r.get("content").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn deps_graph_default_format_is_json_object() {
        let ws = empty_ws();
        let resp = dispatch_deps_graph(&ws, 2, &serde_json::json!({}));
        assert!(resp.error.is_none());
        // JSON branch serialises the graph struct (not the {format,content} shape).
        let r = resp.result.expect("result");
        assert!(
            r.get("content").is_none(),
            "json branch must not carry the dot `content` field"
        );
    }
}
