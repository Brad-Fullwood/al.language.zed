//! The dependency source index: a summary of every AL file embedded in the
//! loaded packages.
//!
//! The call graph and transaction lint are the only readers, and they read
//! what [`SourceFileSummary`] keeps. Each file is parsed once, summarized,
//! and its tree dropped, so the index holds neither trees nor source text.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_insight::calls::{SourceFileSummary, SummarizedFile};

use crate::DependencySourceError;

/// The summarized embedded AL of one package.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageSourceSummary {
    /// One summary per embedded file that parsed cleanly and declares an
    /// object, in archive order.
    pub files: Vec<SourceFileSummary>,
    /// Embedded `.al` files left out because they did not parse cleanly or
    /// declared no object.
    pub skipped_files: usize,
}

impl PackageSourceSummary {
    /// Parse and summarize every embedded AL file of the package at
    /// `app_path`, on the rayon pool. `on_file` runs once per summarized file.
    pub fn build(
        app_path: &Path,
        on_file: impl Fn() + Sync,
    ) -> Result<Self, DependencySourceError> {
        use rayon::prelude::*;

        let source_index = al_symbols::source_index::get_or_build(app_path).map_err(|source| {
            DependencySourceError::IndexPackage {
                path: app_path.to_path_buf(),
                source,
            }
        })?;
        let sources = source_index.extract_all_sources().map_err(|source| {
            DependencySourceError::ExtractPackage {
                path: app_path.to_path_buf(),
                source,
            }
        })?;
        let summaries: Vec<Option<SourceFileSummary>> = sources
            .into_par_iter()
            .map(|(archive_path, source)| {
                let summary = summarize_embedded(app_path, archive_path, &source);
                if summary.is_some() {
                    on_file();
                }
                summary
            })
            .collect();
        let skipped_files = summaries.iter().filter(|summary| summary.is_none()).count();
        Ok(Self {
            files: summaries.into_iter().flatten().collect(),
            skipped_files,
        })
    }

    /// Bytes the summaries own. See [`SourceFileSummary::heap_bytes`].
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .files
                .iter()
                .map(SourceFileSummary::heap_bytes)
                .sum::<usize>()
    }
}

/// Parse one embedded file and summarize it, or `None` when it is skipped.
///
/// Degrade per file: one odd embedded `.al` (a grammar gap for a newer AL
/// construct, a namespace-only file, a vendor's scratch file) must not
/// disable call-graph and insight features for the whole workspace.
fn summarize_embedded(
    app_path: &Path,
    archive_path: String,
    source: &str,
) -> Option<SourceFileSummary> {
    let parsed = al_syntax::AlParser::parse_quick(source);
    if !parsed.errors.is_empty() {
        let details = parsed
            .errors
            .iter()
            .take(3)
            .map(|error| {
                format!(
                    "{} at {}:{}",
                    error.message,
                    error.range.start_point.row + 1,
                    error.range.start_point.column + 1
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        tracing::warn!(
            package = %app_path.display(),
            archive_path = %archive_path,
            details = %details,
            "dependency source index: skipping embedded AL that does not parse cleanly"
        );
        return None;
    }
    if al_syntax::find_object_declaration(&parsed.tree, source).is_none() {
        tracing::debug!(
            package = %app_path.display(),
            archive_path = %archive_path,
            "dependency source index: skipping declaration-free embedded AL"
        );
        return None;
    }
    Some(SourceFileSummary::from_tree(
        archive_path,
        &parsed.tree,
        source,
    ))
}

/// Every loaded package's summarized source.
#[derive(Debug, Default)]
pub struct DependencySources {
    packages: Vec<(PathBuf, Arc<PackageSourceSummary>)>,
    /// `(path the graph reports, package, file)`, sorted by path. A path two
    /// files map to keeps the later file, as the parsed index did.
    order: Vec<(PathBuf, usize, usize)>,
}

impl DependencySources {
    pub fn new(packages: Vec<(PathBuf, Arc<PackageSourceSummary>)>) -> Self {
        let mut by_path: BTreeMap<PathBuf, (usize, usize)> = BTreeMap::new();
        for (package_index, (app_path, package)) in packages.iter().enumerate() {
            for (file_index, file) in package.files.iter().enumerate() {
                by_path.insert(
                    dependency_virtual_path(app_path, &file.archive_path),
                    (package_index, file_index),
                );
            }
        }
        let order = by_path
            .into_iter()
            .map(|(path, (package, file))| (path, package, file))
            .collect();
        Self { packages, order }
    }

    /// How many summarized files the index holds.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Every file under the path the graph reports it by, sorted by path.
    pub fn files(&self) -> Vec<SummarizedFile<'_>> {
        self.order
            .iter()
            .map(|(path, package, file)| (path.as_path(), &self.packages[*package].1.files[*file]))
            .collect()
    }

    /// The packages, each with its summary.
    pub fn packages(&self) -> &[(PathBuf, Arc<PackageSourceSummary>)] {
        &self.packages
    }

    pub fn memory_stats(&self) -> DependencySourceMemoryStats {
        let summary_bytes = self
            .packages
            .iter()
            .map(|(path, package)| path.as_os_str().len() + package.heap_bytes())
            .sum();
        let order_bytes = self
            .order
            .iter()
            .map(|(path, _, _)| {
                std::mem::size_of::<(PathBuf, usize, usize)>() + path.as_os_str().len()
            })
            .sum::<usize>();
        DependencySourceMemoryStats {
            summary_bytes,
            tracked_bytes: summary_bytes + order_bytes,
        }
    }
}

/// Bytes the dependency source index owns.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencySourceMemoryStats {
    /// The summaries themselves.
    pub summary_bytes: usize,
    /// Summaries plus the path index over them.
    pub tracked_bytes: usize,
}

/// The path the graph reports an embedded file by: stable within one process,
/// distinct per package.
pub(crate) fn dependency_virtual_path(app_path: &Path, archive_path: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    app_path.hash(&mut hasher);
    let mut path = PathBuf::from("/__al_dependency_sources__");
    path.push(format!("{:016x}", hasher.finish()));
    let component_count_before = path.components().count();
    for component in Path::new(archive_path).components() {
        if let std::path::Component::Normal(component) = component {
            path.push(component);
        }
    }
    if path.components().count() == component_count_before {
        path.push("source.al");
    }
    path
}
