//! Dependency graph built from the typed project manifest and loaded `.app` manifests.
//!
//! App GUID is the primary identity. Display names and publishers remain in the
//! response for humans, but never decide whether a package satisfies a
//! dependency when a GUID is available.

use al_project::project::AppManifest;
use al_types::AppDependency;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub dependencies: Vec<AppDependency>,
    pub source_path: String,
}

impl PackageEntry {
    pub fn from_manifest(manifest: al_symbols::NavxManifest, source_path: String) -> Self {
        Self {
            app_id: manifest.app_id,
            name: manifest.name,
            publisher: manifest.publisher,
            version: manifest.version,
            dependencies: manifest
                .dependencies
                .into_iter()
                .map(|dependency| AppDependency {
                    id: dependency.app_id,
                    name: dependency.name,
                    publisher: dependency.publisher,
                    version: dependency.min_version,
                })
                .collect(),
            source_path,
        }
    }

    fn identity(&self) -> String {
        identity_key(&self.app_id, &self.name, &self.publisher)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepNode {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_path: Option<String>,
}

impl DepNode {
    fn identity(&self) -> String {
        identity_key(&self.app_id, &self.name, &self.publisher)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepEdge {
    pub from_app_id: String,
    pub from: String,
    pub from_publisher: String,
    pub to_app_id: String,
    pub to: String,
    pub to_publisher: String,
    pub required_version: String,
}

impl DepEdge {
    fn source_identity(&self) -> String {
        identity_key(&self.from_app_id, &self.from, &self.from_publisher)
    }

    fn to_identity(&self) -> String {
        identity_key(&self.to_app_id, &self.to, &self.to_publisher)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionConflict {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub loaded_versions: Vec<String>,
    pub required_versions: Vec<String>,
    pub required_by: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    pub fn to_dot(&self) -> String {
        let mut output =
            String::from("digraph AL_Dependencies {\n  rankdir=LR;\n  node [shape=box];\n");
        let root_identity = self.root_app.identity();
        output.push_str(&format!(
            "  \"{}\" [label=\"{}\\n{}\\n{}\", style=filled, fillcolor=lightblue];\n",
            dot_escape(&root_identity),
            dot_escape(&self.root_app.name),
            dot_escape(&self.root_app.publisher),
            dot_escape(&self.root_app.version),
        ));

        let conflict_identities = self
            .conflicts
            .iter()
            .map(|conflict| identity_key(&conflict.app_id, &conflict.name, &conflict.publisher))
            .collect::<HashSet<_>>();
        let loaded_identities = self
            .nodes
            .iter()
            .map(DepNode::identity)
            .collect::<HashSet<_>>();

        for node in &self.nodes {
            let identity = node.identity();
            let conflict_style = if conflict_identities.contains(&identity) {
                ", style=filled, fillcolor=mistyrose"
            } else {
                ""
            };
            output.push_str(&format!(
                "  \"{}\" [label=\"{}\\n{}\\n{}\"{}];\n",
                dot_escape(&identity),
                dot_escape(&node.name),
                dot_escape(&node.publisher),
                dot_escape(&node.version),
                conflict_style,
            ));
        }

        let mut rendered_missing = HashSet::new();
        for edge in &self.edges {
            let target = edge.to_identity();
            if !loaded_identities.contains(&target) && rendered_missing.insert(target.clone()) {
                output.push_str(&format!(
                    "  \"{}\" [label=\"{}\\n{}\\nmissing\", style=\"dashed,filled\", fillcolor=mistyrose];\n",
                    dot_escape(&target),
                    dot_escape(&edge.to),
                    dot_escape(&edge.to_publisher),
                ));
            }
        }

        for edge in &self.edges {
            output.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\">={}\"];\n",
                dot_escape(&edge.source_identity()),
                dot_escape(&edge.to_identity()),
                dot_escape(&edge.required_version),
            ));
        }
        output.push_str("}\n");
        output
    }
}

pub fn build_dependency_graph(
    manifest: &AppManifest,
    root_dependencies: &[AppDependency],
    packages: &[PackageEntry],
) -> DependencyGraph {
    let root_app = DepNode {
        app_id: manifest.id.clone(),
        name: manifest.name.clone(),
        publisher: manifest.publisher.clone(),
        version: manifest.version.clone(),
        package_path: None,
    };

    let mut packages = packages.to_vec();
    packages.sort_by(|left, right| {
        (
            left.identity(),
            left.version.to_ascii_lowercase(),
            left.source_path.to_ascii_lowercase(),
        )
            .cmp(&(
                right.identity(),
                right.version.to_ascii_lowercase(),
                right.source_path.to_ascii_lowercase(),
            ))
    });

    let nodes = packages
        .iter()
        .map(|package| DepNode {
            app_id: package.app_id.clone(),
            name: package.name.clone(),
            publisher: package.publisher.clone(),
            version: package.version.clone(),
            package_path: Some(package.source_path.clone()),
        })
        .collect::<Vec<_>>();

    let root_source = (
        manifest.id.as_str(),
        manifest.name.as_str(),
        manifest.publisher.as_str(),
    );
    let mut edges = root_dependencies
        .iter()
        .map(|dependency| edge(root_source, dependency))
        .collect::<Vec<_>>();
    for package in &packages {
        let source = (
            package.app_id.as_str(),
            package.name.as_str(),
            package.publisher.as_str(),
        );
        edges.extend(
            package
                .dependencies
                .iter()
                .map(|dependency| edge(source, dependency)),
        );
    }
    edges.sort_by(|left, right| {
        (
            left.source_identity(),
            left.to_identity(),
            left.required_version.to_ascii_lowercase(),
        )
            .cmp(&(
                right.source_identity(),
                right.to_identity(),
                right.required_version.to_ascii_lowercase(),
            ))
    });

    let mut packages_by_identity = BTreeMap::<String, Vec<&PackageEntry>>::new();
    for package in &packages {
        packages_by_identity
            .entry(package.identity())
            .or_default()
            .push(package);
    }

    let direct = root_dependencies
        .iter()
        .map(dependency_identity)
        .collect::<BTreeSet<_>>();
    let root_identity = root_app.identity();
    let mut visited = direct.clone();
    let mut queue = direct.iter().cloned().collect::<VecDeque<_>>();
    let mut transitive_identities = BTreeSet::new();
    while let Some(identity) = queue.pop_front() {
        let Some(candidates) = packages_by_identity.get(&identity) else {
            continue;
        };
        for package in candidates {
            for dependency in &package.dependencies {
                let target = dependency_identity(dependency);
                if target == root_identity {
                    continue;
                }
                if visited.insert(target.clone()) && packages_by_identity.contains_key(&target) {
                    transitive_identities.insert(target.clone());
                    queue.push_back(target);
                }
            }
        }
    }
    let mut transitive = nodes
        .iter()
        .filter(|node| transitive_identities.contains(&node.identity()))
        .cloned()
        .collect::<Vec<_>>();
    transitive.sort_by(|left, right| {
        (left.identity(), left.version.to_ascii_lowercase())
            .cmp(&(right.identity(), right.version.to_ascii_lowercase()))
    });

    let mut missing = edges
        .iter()
        .filter(|edge| !packages_by_identity.contains_key(&edge.to_identity()))
        .map(|edge| {
            format!(
                "{} / {} ({}, >= {}) required by {} / {}",
                edge.to_publisher,
                edge.to,
                edge.to_app_id,
                edge.required_version,
                edge.from_publisher,
                edge.from
            )
        })
        .collect::<Vec<_>>();
    missing.sort();
    missing.dedup();

    let conflicts = find_version_conflicts(&edges, &packages_by_identity);

    DependencyGraph {
        root_app,
        nodes,
        edges,
        transitive,
        conflicts,
        missing,
    }
}

fn edge(source: (&str, &str, &str), dependency: &AppDependency) -> DepEdge {
    DepEdge {
        from_app_id: source.0.to_string(),
        from: source.1.to_string(),
        from_publisher: source.2.to_string(),
        to_app_id: dependency.id.clone(),
        to: dependency.name.clone(),
        to_publisher: dependency.publisher.clone(),
        required_version: dependency.version.clone(),
    }
}

fn identity_key(app_id: &str, name: &str, publisher: &str) -> String {
    let app_id = app_id.trim();
    if !app_id.is_empty() {
        format!("id:{}", app_id.to_ascii_lowercase())
    } else {
        format!(
            "name:{}\u{0}{}",
            publisher.trim().to_ascii_lowercase(),
            name.trim().to_ascii_lowercase()
        )
    }
}

fn dependency_identity(dependency: &AppDependency) -> String {
    identity_key(&dependency.id, &dependency.name, &dependency.publisher)
}

fn find_version_conflicts(
    edges: &[DepEdge],
    packages_by_identity: &BTreeMap<String, Vec<&PackageEntry>>,
) -> Vec<VersionConflict> {
    let mut requirements = BTreeMap::<String, Vec<&DepEdge>>::new();
    for edge in edges {
        requirements
            .entry(edge.to_identity())
            .or_default()
            .push(edge);
    }

    let mut identities = packages_by_identity
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    identities.extend(requirements.keys().cloned());
    let mut conflicts = Vec::new();
    for identity in identities {
        let loaded = packages_by_identity
            .get(&identity)
            .cloned()
            .unwrap_or_default();
        if loaded.is_empty() {
            continue;
        }
        let required = requirements.get(&identity).cloned().unwrap_or_default();
        let mut loaded_versions = loaded
            .iter()
            .map(|package| package.version.clone())
            .collect::<Vec<_>>();
        loaded_versions.sort();
        loaded_versions.dedup();
        let mut required_versions = required
            .iter()
            .map(|edge| edge.required_version.clone())
            .filter(|version| !version.is_empty() && version != "*")
            .collect::<Vec<_>>();
        required_versions.sort();
        required_versions.dedup();

        let unsatisfied = required.iter().any(|edge| {
            !edge.required_version.is_empty()
                && edge.required_version != "*"
                && !loaded.iter().any(|package| {
                    al_symbols::model::version_at_least(&package.version, &edge.required_version)
                })
        });
        let inconsistent_identity_metadata = loaded.iter().skip(1).any(|package| {
            !package.name.eq_ignore_ascii_case(&loaded[0].name)
                || !package.publisher.eq_ignore_ascii_case(&loaded[0].publisher)
        });

        let mut reasons = Vec::new();
        if loaded_versions.len() > 1 {
            reasons.push("multiple package versions are loaded for one app identity");
        }
        if unsatisfied {
            reasons.push("no loaded package version satisfies every minimum-version requirement");
        }
        if inconsistent_identity_metadata {
            reasons.push("one app identity has inconsistent name or publisher metadata");
        }
        if reasons.is_empty() {
            continue;
        }

        let identity_edge = required.first().copied();
        let app_id = identity_edge
            .map(|edge| edge.to_app_id.clone())
            .unwrap_or_else(|| loaded[0].app_id.clone());
        let name = identity_edge
            .map(|edge| edge.to.clone())
            .unwrap_or_else(|| loaded[0].name.clone());
        let publisher = identity_edge
            .map(|edge| edge.to_publisher.clone())
            .unwrap_or_else(|| loaded[0].publisher.clone());
        let mut required_by = required
            .iter()
            .map(|edge| format!("{} / {}", edge.from_publisher, edge.from))
            .collect::<Vec<_>>();
        required_by.sort();
        required_by.dedup();
        conflicts.push(VersionConflict {
            app_id,
            name,
            publisher,
            loaded_versions,
            required_versions,
            required_by,
            reason: reasons.join("; "),
        });
    }
    conflicts.sort_by(|left, right| {
        identity_key(&left.app_id, &left.name, &left.publisher).cmp(&identity_key(
            &right.app_id,
            &right.name,
            &right.publisher,
        ))
    });
    conflicts
}

fn dot_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(dependencies: Vec<AppDependency>) -> AppManifest {
        AppManifest {
            id: "root-id".to_string(),
            name: "My App".to_string(),
            publisher: "Me".to_string(),
            version: "1.0.0.0".to_string(),
            dependencies,
            application: None,
            platform: None,
            runtime: None,
        }
    }

    fn dependency(id: &str, name: &str, publisher: &str, version: &str) -> AppDependency {
        AppDependency {
            id: id.to_string(),
            name: name.to_string(),
            publisher: publisher.to_string(),
            version: version.to_string(),
        }
    }

    fn package(
        id: &str,
        name: &str,
        publisher: &str,
        version: &str,
        dependencies: Vec<AppDependency>,
    ) -> PackageEntry {
        PackageEntry {
            app_id: id.to_string(),
            name: name.to_string(),
            publisher: publisher.to_string(),
            version: version.to_string(),
            dependencies,
            source_path: format!(".alpackages/{name}_{version}.app"),
        }
    }

    #[test]
    fn builds_guid_matched_transitive_graph() {
        let direct = dependency("base-id", "Base Application", "Microsoft", "24.0.0.0");
        let transitive = dependency("system-id", "System Application", "Microsoft", "24.0.0.0");
        let root = manifest(vec![direct.clone()]);
        let packages = vec![
            package(
                "base-id",
                "Base Application",
                "Microsoft",
                "24.1.0.0",
                vec![transitive],
            ),
            package(
                "system-id",
                "System Application",
                "Microsoft",
                "24.0.0.0",
                vec![],
            ),
        ];
        let graph = build_dependency_graph(&root, &root.dependencies, &packages);
        assert!(graph.missing.is_empty());
        assert!(graph.conflicts.is_empty());
        assert_eq!(graph.transitive.len(), 1);
        assert_eq!(graph.transitive[0].app_id, "system-id");
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from_app_id == "base-id" && edge.to_app_id == "system-id"));
    }

    #[test]
    fn same_display_name_with_distinct_guids_does_not_collide() {
        let root = manifest(vec![]);
        let packages = vec![
            package("a", "Shared Lib", "Vendor", "1.0.0.0", vec![]),
            package("b", "Shared Lib", "Vendor", "1.0.0.0", vec![]),
        ];
        let graph = build_dependency_graph(&root, &[], &packages);
        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.conflicts.is_empty());
    }

    #[test]
    fn missing_dependency_is_explicit_and_identified() {
        let missing = dependency("ghost-id", "Ghost", "Vendor", "2.0.0.0");
        let root = manifest(vec![missing]);
        let graph = build_dependency_graph(&root, &root.dependencies, &[]);
        assert_eq!(graph.missing.len(), 1);
        assert!(graph.missing[0].contains("ghost-id"));
        assert!(graph.missing[0].contains(">= 2.0.0.0"));
    }

    #[test]
    fn compatible_minimum_versions_are_not_a_conflict() {
        let shared_v1 = dependency("shared", "Shared", "Vendor", "1.0.0.0");
        let shared_v2 = dependency("shared", "Shared", "Vendor", "2.0.0.0");
        let root = manifest(vec![dependency("a", "A", "Vendor", "1.0.0.0")]);
        let packages = vec![
            package("a", "A", "Vendor", "1.0.0.0", vec![shared_v1]),
            package("shared", "Shared", "Vendor", "2.1.0.0", vec![shared_v2]),
        ];
        let graph = build_dependency_graph(&root, &root.dependencies, &packages);
        assert!(graph.conflicts.is_empty(), "{:?}", graph.conflicts);
    }

    #[test]
    fn unsatisfied_minimum_version_is_a_conflict() {
        let root = manifest(vec![dependency("shared", "Shared", "Vendor", "3.0.0.0")]);
        let packages = vec![package("shared", "Shared", "Vendor", "2.9.0.0", vec![])];
        let graph = build_dependency_graph(&root, &root.dependencies, &packages);
        assert_eq!(graph.conflicts.len(), 1);
        assert!(graph.conflicts[0].reason.contains("minimum-version"));
    }

    #[test]
    fn duplicate_loaded_versions_are_a_conflict() {
        let root = manifest(vec![]);
        let packages = vec![
            package("shared", "Shared", "Vendor", "1.0.0.0", vec![]),
            package("shared", "Shared", "Vendor", "2.0.0.0", vec![]),
        ];
        let graph = build_dependency_graph(&root, &[], &packages);
        assert_eq!(graph.conflicts.len(), 1);
        assert_eq!(
            graph.conflicts[0].loaded_versions,
            vec!["1.0.0.0", "2.0.0.0"]
        );
    }

    #[test]
    fn dot_uses_guid_identity_and_escapes_labels() {
        let mut root = manifest(vec![]);
        root.name = "Quoted \"App\"".to_string();
        let graph = build_dependency_graph(&root, &[], &[]);
        let dot = graph.to_dot();
        assert!(dot.contains("\"id:root-id\""));
        assert!(dot.contains("Quoted \\\"App\\\""));
    }

    #[test]
    fn graph_order_is_deterministic_for_reversed_package_input() {
        let root = manifest(vec![]);
        let a = package("a", "A", "Vendor", "1.0.0.0", vec![]);
        let b = package("b", "B", "Vendor", "1.0.0.0", vec![]);
        let first = build_dependency_graph(&root, &[], &[a.clone(), b.clone()]);
        let second = build_dependency_graph(&root, &[], &[b, a]);
        assert_eq!(first, second);
    }
}
