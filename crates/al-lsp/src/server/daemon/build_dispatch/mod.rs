//! Build/toolchain/analysis dispatchers — compile, package, lint, format, fix, permissions,
//! authenticate, download symbols, snapshot, profiling, xliff, etc.

mod build;
mod codegen;
mod fixes;
mod symbols_auth;
mod tests_dispatch;
mod xliff;

pub(super) use build::*;
pub(super) use codegen::*;
pub(super) use fixes::*;
pub(super) use symbols_auth::*;
pub(super) use tests_dispatch::*;
pub(super) use xliff::*;

use crate::workspace::Workspace;
use al_protocol::jsonrpc::Response;

pub(super) const ERR_INITIALIZING: &str = "Workspace is initializing, try again";
pub(super) const ERR_NO_PROJECT: &str = "No project loaded";

/// Largest realistic AL procedure body is ~5k tokens; cap the
/// `minTokens` duplicate-detection threshold at 10k so a hostile or
/// fat-fingered client can't (a) push the threshold above any real
/// procedure (effectively disabling detection) or (b) drive the
/// scan loop into pathological territory. F-OPEN-007.
const MAX_DUPLICATES_MIN_TOKENS: u64 = 10_000;

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

/// Extract a baseline symbol set from a JSON-RPC `params.baselineSymbols`
/// array. Each element is deserialized as a [`SymbolEntry`]; malformed entries
/// are skipped (logged at WARN) rather than failing the whole request, and an
/// absent/non-array field yields an empty baseline so the call stays backward-
/// compatible (A5/A6). The expected shape matches the wire form produced by
/// the `symbols` daemon method (and by `analyze_breaking_changes` callers).
fn baseline_symbols_from_params(params: &serde_json::Value) -> Vec<crate::symbols::SymbolEntry> {
    let Some(arr) = params.get("baselineSymbols").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut baseline = Vec::with_capacity(arr.len());
    for (i, value) in arr.iter().enumerate() {
        match serde_json::from_value::<crate::symbols::SymbolEntry>(value.clone()) {
            Ok(entry) => baseline.push(entry),
            Err(e) => {
                tracing::warn!(
                    index = i,
                    error = %e,
                    "baselineSymbols[{i}] could not be deserialized as a SymbolEntry — skipping"
                );
            }
        }
    }
    baseline
}

pub(super) fn dispatch_breaking_changes(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // A5: callers supply the previous version's symbols in
    // `params.baselineSymbols`; an absent baseline finds every current symbol
    // as new (the pre-fix behaviour). Populating it lets removals/changes be
    // reported against a real previous version.
    let current: Vec<crate::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline = baseline_symbols_from_params(params);
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
    params: &serde_json::Value,
) -> Response {
    // A6: callers supply the previous version's symbols in
    // `params.baselineSymbols` (e.g. extracted from a previous `.app`). An
    // absent baseline finds all current symbols as new/changed; populating it
    // produces a real upgrade-impact report against the previous version.
    let current: Vec<crate::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline = baseline_symbols_from_params(params);
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

    #[test]
    fn clamp_min_tokens_defaults_when_absent() {
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
        assert!((clamp_min_similarity(Some(-1.0)) - 0.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(2.5)) - 1.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(1e308)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_rejects_non_finite() {
        assert!((clamp_min_similarity(Some(f64::NAN)) - 0.8).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(f64::INFINITY)) - 0.8).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(f64::NEG_INFINITY)) - 0.8).abs() < 1e-6);
    }

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
        let r = resp.result.expect("result");
        assert!(
            r.get("content").is_none(),
            "json branch must not carry the dot `content` field"
        );
    }

    // =======================================================================
    // A5/A6: breaking-change & upgrade baseline plumbing.
    //
    // These exercise the real `dispatch_breaking_changes` /
    // `dispatch_upgrade_report` against a SYNTHETIC in-memory baseline supplied
    // via `params.baselineSymbols` and a workspace symbol index populated with
    // `add_entries_owned`. No ALTool / BC server is required.
    // needsAltoolForLiveE2e=false.
    // =======================================================================
    use crate::symbols::{MethodSymbol, ObjectKind, SymbolEntry};

    fn codeunit(name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: name.to_string(),
            package: "Test".to_string(),
            methods,
            ..Default::default()
        }
    }

    fn public_method(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }
    }

    /// Serialize a symbol set into the `baselineSymbols` JSON-RPC param shape.
    fn params_with_baseline(baseline: &[SymbolEntry]) -> serde_json::Value {
        serde_json::json!({ "baselineSymbols": baseline })
    }

    #[test]
    fn baseline_symbols_absent_yields_empty() {
        // No baselineSymbols field -> empty baseline (backward compatible).
        assert!(baseline_symbols_from_params(&serde_json::json!({})).is_empty());
        // Non-array value is ignored rather than erroring.
        assert!(
            baseline_symbols_from_params(&serde_json::json!({ "baselineSymbols": 7 })).is_empty()
        );
    }

    #[test]
    fn baseline_symbols_round_trip_and_skips_malformed() {
        // Valid entries deserialize; a malformed entry is skipped, not fatal.
        let valid = codeunit("My CU", vec![public_method("DoWork")]);
        let params = serde_json::json!({
            "baselineSymbols": [
                serde_json::to_value(&valid).unwrap(),
                serde_json::json!({ "kind": 123, "totally": "wrong" }),
            ]
        });
        let parsed = baseline_symbols_from_params(&params);
        assert_eq!(
            parsed.len(),
            1,
            "malformed entry must be skipped: {parsed:?}"
        );
        assert_eq!(parsed[0].name, "My CU");
    }

    #[test]
    fn breaking_changes_absent_baseline_reports_nothing() {
        // The pre-fix behaviour: with no baseline, added current symbols are
        // not breaking, so the result is empty.
        let ws = empty_ws();
        ws.symbols
            .add_entries_owned(vec![codeunit("New CU", vec![public_method("DoWork")])]);
        let resp = dispatch_breaking_changes(&ws, 1, &serde_json::json!({}));
        assert!(resp.error.is_none());
        let arr = resp.result.expect("result");
        assert_eq!(
            arr.as_array().map(|a| a.len()),
            Some(0),
            "absent baseline must report no breaking changes: {arr:?}"
        );
    }

    #[test]
    fn breaking_changes_detects_removed_object_from_baseline() {
        // A5: baseline has an object the current workspace no longer contains.
        let ws = empty_ws(); // current symbol set is empty
        let baseline = vec![codeunit("Old CU", vec![])];
        let resp = dispatch_breaking_changes(&ws, 2, &params_with_baseline(&baseline));
        assert!(resp.error.is_none());
        let arr = resp.result.expect("result");
        let changes = arr.as_array().expect("array");
        assert!(
            changes
                .iter()
                .any(|c| c["kind"] == "objectRemoved" && c["object"] == "Old CU"),
            "removed object must be reported: {changes:?}"
        );
    }

    #[test]
    fn breaking_changes_detects_removed_procedure_from_baseline() {
        // A5: object survives but a public procedure was removed.
        let ws = empty_ws();
        ws.symbols
            .add_entries_owned(vec![codeunit("My CU", vec![])]);
        let baseline = vec![codeunit("My CU", vec![public_method("DoWork")])];
        let resp = dispatch_breaking_changes(&ws, 3, &params_with_baseline(&baseline));
        let arr = resp.result.expect("result");
        let changes = arr.as_array().expect("array");
        assert!(
            changes
                .iter()
                .any(|c| c["kind"] == "procedureRemoved" && c["member"] == "DoWork"),
            "removed procedure must be reported: {changes:?}"
        );
    }

    #[test]
    fn breaking_changes_identical_baseline_reports_nothing() {
        // A5: an identical baseline produces no breaking changes.
        let entry = codeunit("Stable CU", vec![public_method("DoWork")]);
        let ws = empty_ws();
        ws.symbols.add_entries_owned(vec![entry.clone()]);
        let resp =
            dispatch_breaking_changes(&ws, 4, &params_with_baseline(std::slice::from_ref(&entry)));
        let arr = resp.result.expect("result");
        assert_eq!(
            arr.as_array().map(|a| a.len()),
            Some(0),
            "identical baseline must report nothing: {arr:?}"
        );
    }

    #[test]
    fn upgrade_report_detects_removed_object_from_baseline() {
        // A6: removed object surfaces as a BreakingChange upgrade issue.
        let ws = empty_ws(); // current empty
        let baseline = vec![codeunit("Legacy CU", vec![])];
        let resp = dispatch_upgrade_report(&ws, 5, &params_with_baseline(&baseline));
        let arr = resp.result.expect("result");
        let issues = arr.as_array().expect("array");
        assert!(
            issues
                .iter()
                .any(|i| i["kind"] == "breakingChange" && i["object"] == "Legacy CU"),
            "removed object must be an upgrade issue: {issues:?}"
        );
    }

    #[test]
    fn upgrade_report_identical_baseline_reports_nothing() {
        // A6: identical baseline => no upgrade issues.
        let entry = codeunit("Stable CU", vec![public_method("DoWork")]);
        let ws = empty_ws();
        ws.symbols.add_entries_owned(vec![entry.clone()]);
        let resp =
            dispatch_upgrade_report(&ws, 6, &params_with_baseline(std::slice::from_ref(&entry)));
        let arr = resp.result.expect("result");
        assert_eq!(
            arr.as_array().map(|a| a.len()),
            Some(0),
            "identical baseline must report no upgrade issues: {arr:?}"
        );
    }
}
