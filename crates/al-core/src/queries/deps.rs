//! Dependency graph with transitive resolution.
//!
//! T1704: Build and query full dependency graph from app.json and .app packages.
//! Includes DOT export, transitive dependencies, and version conflict detection.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

/// A package entry: (name, publisher, version, transitive-dependencies).
///
/// Each dependency tuple is (dep_name, dep_publisher, required_version).
pub type PackageEntry = (String, String, String, Vec<(String, String, String)>);

/// A single dependency node in the graph.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepNode {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// An edge in the dependency graph.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepEdge {
    pub from: String,
    pub to: String,
    pub required_version: String,
}

/// Version conflict: multiple versions of the same package required.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionConflict {
    pub name: String,
    pub versions: Vec<String>,
    pub required_by: Vec<String>,
}

/// Full dependency graph.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyGraph {
    pub root_app: DepNode,
    pub nodes: Vec<DepNode>,
    pub edges: Vec<DepEdge>,
    pub transitive: Vec<DepNode>,
    pub conflicts: Vec<VersionConflict>,
    pub missing: Vec<String>,
}

impl DependencyGraph {
    /// Export the graph in DOT format for Graphviz.
    pub fn to_dot(&self) -> String {
        let mut s = String::from("digraph AL_Dependencies {\n  rankdir=LR;\n  node [shape=box];\n");
        s.push_str(&format!("  \"{}\" [style=filled, fillcolor=lightblue];\n", self.root_app.name));

        for node in &self.nodes {
            s.push_str(&format!("  \"{}\" [label=\"{}\\n{}\"];\n",
                node.name, node.name, node.version));
        }

        for edge in &self.edges {
            s.push_str(&format!("  \"{}\" -> \"{}\" [label=\"{}\"];\n",
                edge.from, edge.to, edge.required_version));
        }

        if !self.conflicts.is_empty() {
            for conflict in &self.conflicts {
                s.push_str(&format!("  \"{}\" [style=filled, fillcolor=red];\n", conflict.name));
            }
        }

        s.push_str("}\n");
        s
    }
}

/// Build a dependency graph from app.json manifest JSON and loaded package info.
///
/// `app_json` is the raw content of the workspace's app.json.
/// `packages` is a list of (name, publisher, version, dependencies_json).
pub fn build_dependency_graph(
    app_json: &str,
    packages: &[PackageEntry],
) -> DependencyGraph {
    // Parse root app info from app.json
    let root_app = parse_root_app(app_json);
    let root_deps = parse_deps_from_app_json(app_json);

    let mut nodes: HashMap<String, DepNode> = HashMap::new();
    let mut edges: Vec<DepEdge> = Vec::new();

    // Add package nodes
    for (name, publisher, version, _) in packages {
        let key = name.to_lowercase();
        nodes.entry(key).or_insert_with(|| DepNode {
            app_id: String::new(),
            name: name.clone(),
            publisher: publisher.clone(),
            version: version.clone(),
        });
    }

    // Add edges from root app
    for (dep_name, _, required_ver) in &root_deps {
        edges.push(DepEdge {
            from: root_app.name.clone(),
            to: dep_name.clone(),
            required_version: required_ver.clone(),
        });
    }

    // Add edges from packages
    for (pkg_name, _, _, pkg_deps) in packages {
        for (dep_name, _, required_ver) in pkg_deps {
            edges.push(DepEdge {
                from: pkg_name.clone(),
                to: dep_name.clone(),
                required_version: required_ver.clone(),
            });
        }
    }

    // Find transitive dependencies (BFS from root)
    let direct: HashSet<String> = root_deps.iter().map(|(n, _, _)| n.to_lowercase()).collect();
    let transitive = find_transitive_deps(&root_app.name, &direct, &edges, &nodes);

    // Find missing dependencies
    let all_node_names: HashSet<String> = nodes.keys().cloned().collect();
    let missing: Vec<String> = edges
        .iter()
        .filter(|e| !all_node_names.contains(&e.to.to_lowercase()))
        .map(|e| e.to.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // Find version conflicts
    let conflicts = find_version_conflicts(&edges, &nodes);

    DependencyGraph {
        root_app,
        nodes: nodes.into_values().collect(),
        edges,
        transitive,
        conflicts,
        missing,
    }
}

fn parse_root_app(app_json: &str) -> DepNode {
    let name = extract_json_string(app_json, "name").unwrap_or_else(|| "Unknown".to_string());
    let publisher = extract_json_string(app_json, "publisher").unwrap_or_default();
    let version = extract_json_string(app_json, "version").unwrap_or_else(|| "0.0.0.0".to_string());
    let app_id = extract_json_string(app_json, "id").unwrap_or_default();
    DepNode { app_id, name, publisher, version }
}

fn parse_deps_from_app_json(app_json: &str) -> Vec<(String, String, String)> {
    // Simple text extraction for "dependencies" array
    let mut deps = Vec::new();
    if let Some(deps_start) = app_json.find("\"dependencies\"") {
        let after = &app_json[deps_start..];
        if let Some(arr_start) = after.find('[') {
            let arr = &after[arr_start..];
            // Find each object in the array
            let mut depth = 0i32;
            let mut obj_start = None;
            for (i, ch) in arr.char_indices() {
                match ch {
                    '{' => {
                        depth += 1;
                        if depth == 1 { obj_start = Some(i); }
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            if let Some(start) = obj_start {
                                let obj_str = &arr[start..=i];
                                let name = extract_json_string(obj_str, "name").unwrap_or_default();
                                let publisher = extract_json_string(obj_str, "publisher").unwrap_or_default();
                                let version = extract_json_string(obj_str, "version").unwrap_or_default();
                                if !name.is_empty() {
                                    deps.push((name, publisher, version));
                                }
                                obj_start = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    deps
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let pos = json.find(&pattern)?;
    let after = json[pos + pattern.len()..].trim_start_matches([' ', ':']);
    if !after.starts_with('"') { return None; }
    let inner = &after[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

fn find_transitive_deps(
    root_name: &str,
    direct: &HashSet<String>,
    edges: &[DepEdge],
    nodes: &HashMap<String, DepNode>,
) -> Vec<DepNode> {
    let mut visited: HashSet<String> = direct.clone();
    let mut queue: VecDeque<String> = direct.iter().cloned().collect();
    let mut transitive = Vec::new();

    while let Some(pkg_name) = queue.pop_front() {
        // Find edges from this package
        for edge in edges {
            if edge.from.to_lowercase() == pkg_name {
                let dep_lower = edge.to.to_lowercase();
                if dep_lower != root_name.to_lowercase() && !visited.contains(&dep_lower) {
                    visited.insert(dep_lower.clone());
                    queue.push_back(dep_lower.clone());
                    if let Some(node) = nodes.get(&dep_lower) {
                        transitive.push(node.clone());
                    }
                }
            }
        }
    }
    transitive
}

fn find_version_conflicts(
    edges: &[DepEdge],
    nodes: &HashMap<String, DepNode>,
) -> Vec<VersionConflict> {
    let mut requirements: HashMap<String, Vec<(String, String)>> = HashMap::new(); // name -> [(requirer, version)]

    for edge in edges {
        if !edge.required_version.is_empty() && edge.required_version != "*" {
            requirements
                .entry(edge.to.to_lowercase())
                .or_default()
                .push((edge.from.clone(), edge.required_version.clone()));
        }
    }

    let mut conflicts = Vec::new();
    for (dep_name, reqs) in &requirements {
        let versions: HashSet<&String> = reqs.iter().map(|(_, v)| v).collect();
        if versions.len() > 1 {
            let actual_name = nodes.get(dep_name)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| dep_name.clone());
            conflicts.push(VersionConflict {
                name: actual_name,
                versions: versions.iter().map(|s| s.to_string()).collect(),
                required_by: reqs.iter().map(|(r, _)| r.clone()).collect(),
            });
        }
    }
    conflicts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_simple_graph() {
        let app_json = r#"{
    "name": "My App",
    "publisher": "Me",
    "version": "1.0.0.0",
    "dependencies": [
        { "name": "Base Application", "publisher": "Microsoft", "version": "24.0.0.0" }
    ]
}"#;

        let packages = vec![
            ("Base Application".to_string(), "Microsoft".to_string(), "24.0.0.0".to_string(), vec![]),
        ];

        let graph = build_dependency_graph(app_json, &packages);
        assert_eq!(graph.root_app.name, "My App");
        assert!(!graph.nodes.is_empty());
        assert!(graph.missing.is_empty(), "No missing deps: {:?}", graph.missing);
    }

    #[test]
    fn detects_missing_dependency() {
        let app_json = r#"{
    "name": "My App",
    "publisher": "Me",
    "version": "1.0.0.0",
    "dependencies": [
        { "name": "Missing Package", "publisher": "Someone", "version": "1.0.0.0" }
    ]
}"#;

        let graph = build_dependency_graph(app_json, &[]);
        assert!(!graph.missing.is_empty(), "Should detect missing dependency");
    }

    #[test]
    fn dot_export_contains_root() {
        let app_json = r#"{"name":"TestApp","publisher":"Me","version":"1.0.0.0"}"#;
        let graph = build_dependency_graph(app_json, &[]);
        let dot = graph.to_dot();
        assert!(dot.contains("TestApp"), "DOT output should contain root app name");
    }

    #[test]
    fn empty_app_json_returns_unknown() {
        let graph = build_dependency_graph("{}", &[]);
        assert_eq!(graph.root_app.name, "Unknown");
    }
}
