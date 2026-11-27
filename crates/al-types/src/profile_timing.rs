//! Per-node times of an AL CPU profile (`.alcpuprofile`), shared by the
//! `analyze` command (al-bc) and the editor hints (al-analysis).
//!
//! Format source: the profile is written by Microsoft's `CpuProfileGenerator`
//! (Microsoft.Dynamics.Nav.EditorServices.Protocol) as `CpuProfile` JSON with a
//! camelCase resolver. `samples[i]` is the node of call frame `i` (one entry per
//! node, not per time sample). `sampleExecutionTimes[i]` is that frame's
//! `AggregatedExecutionTime`, which is **inclusive**: callees are added to every
//! caller (CpuProfileGenerator.cs:475-483, 679-700). `timeDeltas` holds absolute
//! differences between neighbouring inclusive times (CpuProfileGenerator.cs:
//! 190-197), so it carries no per node time and is not read here.
//!
//! A Sampling profile (`kind` 1) comes from the server instead, with no
//! `sampleExecutionTimes`. The AL extension (18.1, `createModelBuilder` and
//! `SamplingModelBuilder` in dist/extension.js) reads it differently: the self
//! time of node `samples[t]` gains `timeDeltas[t]`, and a node's total is its
//! self time plus its children's totals. Any other `kind` uses the
//! instrumentation reading above. This module does the same.
//!
//! Times are microseconds. A Sampling capture on the CDX Sandbox (2026-11-27,
//! `testdata/cdx-sampling.alcpuprofile`) has `startTime` 63931369939557935,
//! microseconds since 0001-01-01, and its `timeDeltas` sum to 91.76 s of a
//! 92.26 s span.

use std::collections::HashMap;

use serde_json::Value;

/// Times of one profile node in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeTime {
    /// Time in the node itself: inclusive time minus the inclusive time of its
    /// children, never below zero.
    pub self_ms: f64,
    /// Inclusive time of the node and everything it called.
    pub total_ms: f64,
}

/// Times for every node id in `nodes`.
///
/// A Sampling profile (`kind` 1) with `samples` and `timeDeltas` gives each
/// node the deltas of its samples as self time. Otherwise, with `samples` and
/// `sampleExecutionTimes` present, the inclusive time of a
/// node is read from `sampleExecutionTimes` and its self time is that minus its
/// children. Without them (a profile with hit counts only) the self time is
/// 1 ms per hit and the total is the roll up over the call tree.
pub fn node_times(profile: &Value, nodes: &[Value]) -> HashMap<u64, NodeTime> {
    let children = children_by_id(nodes);
    if let Some(self_ms) = sampled_self_by_node(profile, nodes) {
        let totals = roll_up_totals(&children, nodes, &self_ms);
        return self_ms
            .iter()
            .map(|(id, own)| {
                let total_ms = totals.get(id).copied().unwrap_or(*own);
                (
                    *id,
                    NodeTime {
                        self_ms: *own,
                        total_ms,
                    },
                )
            })
            .collect();
    }
    match inclusive_by_node(profile) {
        Some(inclusive_ms) => nodes
            .iter()
            .filter_map(|node| node.get("id").and_then(Value::as_u64))
            .map(|id| {
                let total_ms = inclusive_ms.get(&id).copied().unwrap_or(0.0);
                let callees: f64 = children
                    .get(&id)
                    .map(|kids| kids.iter().filter_map(|k| inclusive_ms.get(k)).sum())
                    .unwrap_or(0.0);
                let self_ms = (total_ms - callees).max(0.0);
                (id, NodeTime { self_ms, total_ms })
            })
            .collect(),
        None => {
            let self_ms: HashMap<u64, f64> = nodes
                .iter()
                .filter_map(|node| {
                    let id = node.get("id").and_then(Value::as_u64)?;
                    let hits = node.get("hitCount").and_then(Value::as_u64).unwrap_or(0);
                    Some((id, hits as f64))
                })
                .collect();
            let totals = roll_up_totals(&children, nodes, &self_ms);
            self_ms
                .iter()
                .map(|(id, own)| {
                    let total_ms = totals.get(id).copied().unwrap_or(*own);
                    (
                        *id,
                        NodeTime {
                            self_ms: *own,
                            total_ms,
                        },
                    )
                })
                .collect()
        }
    }
}

/// Self time per node id in ms for a Sampling profile: `timeDeltas[t]` is
/// added to node `samples[t]`. `None` when `kind` is not 1 or the arrays are
/// missing. A length mismatch uses the common prefix.
fn sampled_self_by_node(profile: &Value, nodes: &[Value]) -> Option<HashMap<u64, f64>> {
    if profile.get("kind").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let samples = profile.get("samples")?.as_array()?;
    let deltas = profile.get("timeDeltas")?.as_array()?;
    let mut by_node: HashMap<u64, f64> = nodes
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_u64))
        .map(|id| (id, 0.0))
        .collect();
    for (sample, delta) in samples.iter().zip(deltas) {
        let (Some(id), Some(us)) = (sample.as_u64(), delta.as_f64()) else {
            continue;
        };
        if let Some(own) = by_node.get_mut(&id) {
            *own += us / 1000.0;
        }
    }
    Some(by_node)
}

/// Inclusive time per node id in ms, or `None` when the profile lacks
/// `samples` or `sampleExecutionTimes`. A length mismatch uses the common prefix.
fn inclusive_by_node(profile: &Value) -> Option<HashMap<u64, f64>> {
    let samples = profile.get("samples")?.as_array()?;
    let times = profile.get("sampleExecutionTimes")?.as_array()?;
    if samples.is_empty() || times.is_empty() {
        return None;
    }
    let mut by_node = HashMap::new();
    for (sample, time) in samples.iter().zip(times) {
        let (Some(id), Some(us)) = (sample.as_u64(), time.as_f64()) else {
            continue;
        };
        by_node.insert(id, us / 1000.0);
    }
    Some(by_node)
}

fn children_by_id(nodes: &[Value]) -> HashMap<u64, Vec<u64>> {
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let kids: Vec<u64> = node
            .get("children")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        children.entry(id).or_default().extend(kids);
    }
    children
}

/// Roll up per node total time (own time plus every descendant) over the
/// `children` adjacency. An iterative post order walk handles several roots and
/// memoizes each node. A cycle or a dangling child id adds 0, so it cannot loop
/// or panic, and a deep tree cannot overflow the native stack.
fn roll_up_totals(
    children: &HashMap<u64, Vec<u64>>,
    nodes: &[Value],
    self_ms_by_node: &HashMap<u64, f64>,
) -> HashMap<u64, f64> {
    use std::collections::HashSet;

    let order: Vec<u64> = nodes
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_u64))
        .collect();

    let mut total: HashMap<u64, f64> = HashMap::new();

    for &start in &order {
        if total.contains_key(&start) {
            continue; // already finished as a descendant of an earlier root
        }
        // Iterative post-order DFS. Each frame is (node_id, children_expanded).
        let mut stack: Vec<(u64, bool)> = vec![(start, false)];
        let mut on_path: HashSet<u64> = HashSet::new();
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                // Children are all finished; sum their memoized totals.
                on_path.remove(&id);
                let mut sum = self_ms_by_node.get(&id).copied().unwrap_or(0.0);
                if let Some(kids) = children.get(&id) {
                    for k in kids {
                        // Back edges (cycles) and dangling ids never entered the
                        // memo table, so they add 0 — no loop, no panic.
                        if let Some(t) = total.get(k) {
                            sum += *t;
                        }
                    }
                }
                total.insert(id, sum);
            } else {
                if total.contains_key(&id) || on_path.contains(&id) {
                    // Finished already, or a back edge into the active path.
                    continue;
                }
                on_path.insert(id);
                stack.push((id, true));
                if let Some(kids) = children.get(&id) {
                    for &k in kids {
                        if !total.contains_key(&k) && !on_path.contains(&k) {
                            stack.push((k, false));
                        }
                    }
                }
            }
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Sampling capture from the CDX Sandbox. IdleTime (node 10) holds the
    /// 90.9 s delta, the SQL chain (16 -> 17) the 5.4 ms one, and node 1 rolls
    /// up the GetSecretFromClient chain, which got a zero delta.
    #[test]
    fn sampling_capture_takes_time_from_time_deltas_like_the_al_extension() {
        let profile: Value =
            serde_json::from_str(include_str!("testdata/cdx-sampling.alcpuprofile")).unwrap();
        let nodes = nodes_of(&profile);
        let times = node_times(&profile, &nodes);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert!(close(times[&10].self_ms, 90_911.784), "{:?}", times[&10]);
        assert!(close(times[&15].self_ms, 843.941), "{:?}", times[&15]);
        assert!(close(times[&17].self_ms, 5.362));
        assert!(close(times[&16].self_ms, 0.0));
        assert!(close(times[&16].total_ms, 5.362));
        assert!(close(times[&9].self_ms, 0.0));
        // Node 2 calls the GetSecretFromClient chain and the SQL chain.
        assert!(close(times[&2].total_ms, 5.362));
        let sum: f64 = times.values().map(|t| t.self_ms).sum();
        assert!(close(sum, 91_761.087), "{sum}");
    }

    #[test]
    fn a_profile_without_kind_1_keeps_the_instrumentation_reading() {
        let mut profile: Value =
            serde_json::from_str(include_str!("testdata/cdx-sampling.alcpuprofile")).unwrap();
        profile["kind"] = serde_json::json!(0);
        let nodes = nodes_of(&profile);
        let times = node_times(&profile, &nodes);
        // No sampleExecutionTimes: the 1 ms per hit fallback.
        assert!((times[&10].self_ms - 826.0).abs() < 1e-9);
    }

    fn node(id: u64, children: &[u64], hits: u64) -> Value {
        json!({ "id": id, "hitCount": hits, "children": children })
    }

    fn nodes_of(profile: &Value) -> Vec<Value> {
        profile["nodes"].as_array().unwrap().clone()
    }

    #[test]
    fn two_children_are_both_subtracted_and_self_never_goes_negative() {
        // Built from CpuProfile / CpuProfileGenerator, not captured.
        // AdjustFrameExecutionTime keeps parent >= sum(children), so 0 is the floor.
        let profile = json!({
            "nodes": [node(1, &[2, 3], 0), node(2, &[], 1), node(3, &[], 1)],
            "samples": [1, 2, 3],
            "sampleExecutionTimes": [9000, 4000, 5000],
            "timeDeltas": [9000, 5000, 1000]
        });
        let times = node_times(&profile, &nodes_of(&profile));
        assert_eq!(times[&1].self_ms, 0.0);
        assert_eq!(times[&1].total_ms, 9.0);
        assert_eq!(times[&2].self_ms, 4.0);
        let short = json!({
            "nodes": [node(1, &[2], 0), node(2, &[], 1)],
            "samples": [1, 2],
            "sampleExecutionTimes": [1000, 4000]
        });
        assert_eq!(node_times(&short, &nodes_of(&short))[&1].self_ms, 0.0);
    }

    #[test]
    fn mismatched_array_lengths_use_the_common_prefix() {
        let profile = json!({
            "nodes": [node(1, &[], 1), node(2, &[], 1)],
            "samples": [1, 2, 2],
            "sampleExecutionTimes": [2000]
        });
        let times = node_times(&profile, &nodes_of(&profile));
        assert_eq!(times[&1].total_ms, 2.0);
        assert_eq!(times[&2].total_ms, 0.0);
    }

    #[test]
    fn hit_count_fallback_rolls_up_and_survives_a_cycle_and_a_dangling_child() {
        // 1 -> 2 -> 1 (back edge) and 1 -> 99 (no such node).
        let profile = json!({
            "nodes": [node(1, &[2, 99], 3), node(2, &[1], 5)]
        });
        let times = node_times(&profile, &nodes_of(&profile));
        assert_eq!(times[&2].total_ms, 5.0);
        assert_eq!(times[&1].total_ms, 8.0);
        assert!(times[&1].total_ms.is_finite());
    }
}
