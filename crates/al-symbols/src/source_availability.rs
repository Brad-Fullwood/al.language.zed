//! Source provenance for package objects.
//!
//! Symbol metadata and original AL source are not interchangeable. These
//! types give every user-facing symbol result a stable, machine-readable
//! description of what navigation can actually provide.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{source_index, SymbolEntry};

/// The strongest source representation available for one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    /// The user's editable `.al` file.
    WorkspaceSource,
    /// Original `.al` source embedded in the package.
    EmbeddedSource,
    /// A navigable declaration outline reconstructed from rich symbol metadata.
    GeneratedOutline,
    /// Only identity-level package metadata is available; there are no members
    /// or source bodies from which to construct a useful API outline.
    MetadataOnly,
}

impl SourceAvailability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::WorkspaceSource => "workspace source",
            Self::EmbeddedSource => "embedded source",
            Self::GeneratedOutline => "generated outline",
            Self::MetadataOnly => "metadata only",
        }
    }
}

/// Per-package object counts by source representation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAvailabilitySummary {
    pub workspace_source: usize,
    pub embedded_source: usize,
    pub generated_outline: usize,
    pub metadata_only: usize,
}

impl SourceAvailabilitySummary {
    pub fn record(&mut self, availability: SourceAvailability) {
        match availability {
            SourceAvailability::WorkspaceSource => self.workspace_source += 1,
            SourceAvailability::EmbeddedSource => self.embedded_source += 1,
            SourceAvailability::GeneratedOutline => self.generated_outline += 1,
            SourceAvailability::MetadataOnly => self.metadata_only += 1,
        }
    }

    pub const fn total(self) -> usize {
        self.workspace_source + self.embedded_source + self.generated_outline + self.metadata_only
    }
}

/// Classify a symbol entry using its already-warmed package source index.
///
/// This function deliberately does not open or scan an archive. Package
/// loaders warm the source-index cache before publishing entries so ordinary
/// search, browse, and package-summary requests stay bounded.
pub fn classify(entry: &SymbolEntry, app_path: Option<&Path>) -> SourceAvailability {
    if entry.package.eq_ignore_ascii_case("workspace")
        || entry.package.eq_ignore_ascii_case("(workspace)")
    {
        return SourceAvailability::WorkspaceSource;
    }

    if let Some(path) = app_path {
        if source_index::get_cached(path)
            .and_then(|index| index.source_path_for_entry(entry).map(str::to_owned))
            .is_some()
        {
            return SourceAvailability::EmbeddedSource;
        }
    }

    classify_metadata(entry)
}

/// Classify the best representation available after original source
/// extraction failed or was not attempted.
pub fn classify_metadata(entry: &SymbolEntry) -> SourceAvailability {
    if has_rich_outline_metadata(entry) {
        SourceAvailability::GeneratedOutline
    } else {
        SourceAvailability::MetadataOnly
    }
}

/// Identity alone can still be displayed and located in package metadata, but
/// calling that an API outline overstates what was shipped. At least one
/// declaration detail must be present before we report `generated_outline`.
pub fn has_rich_outline_metadata(entry: &SymbolEntry) -> bool {
    entry.extends.is_some()
        || !entry.implements.is_empty()
        || !entry.methods.is_empty()
        || !entry.fields.is_empty()
        || !entry.controls.is_empty()
        || !entry.enum_values.is_empty()
        || !entry.keys.is_empty()
        || !entry.properties.is_empty()
        || !entry.variables.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldSymbol, ObjectKind};
    use std::io::{Cursor, Write};

    fn write_source_app(source: &str) -> tempfile::NamedTempFile {
        let mut zip_bytes = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_bytes);
            let mut zip = zip::ZipWriter::new(cursor);
            zip.start_file(
                "src/Embedded.Table.al",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(source.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let mut app = tempfile::NamedTempFile::new().unwrap();
        app.write_all(b"NAVX").unwrap();
        app.write_all(&1u32.to_le_bytes()).unwrap();
        app.write_all(&40u32.to_le_bytes()).unwrap();
        app.write_all(&[0u8; 28]).unwrap();
        app.write_all(&zip_bytes).unwrap();
        app.flush().unwrap();
        app
    }

    #[test]
    fn workspace_entries_are_workspace_source() {
        let entry = SymbolEntry {
            package: "workspace".into(),
            ..Default::default()
        };
        assert_eq!(classify(&entry, None), SourceAvailability::WorkspaceSource);
    }

    #[test]
    fn rich_metadata_and_identity_only_are_distinct() {
        let bare = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Bare".into(),
            package: "Pkg".into(),
            ..Default::default()
        };
        assert_eq!(classify(&bare, None), SourceAvailability::MetadataOnly);

        let rich = SymbolEntry {
            fields: vec![FieldSymbol {
                id: 1,
                name: "Code".into(),
                type_name: "Code[20]".into(),
                properties: Vec::new(),
            }],
            ..bare
        };
        assert_eq!(classify(&rich, None), SourceAvailability::GeneratedOutline);
    }

    #[test]
    fn warmed_package_index_reports_embedded_source_without_request_time_scan() {
        let app = write_source_app("table 50100 Embedded { }");
        let canonical = std::fs::canonicalize(app.path()).unwrap();
        source_index::get_or_build(&canonical).expect("warm source index");
        let entry = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50_100,
            name: "Embedded".to_string(),
            package: "Source App".to_string(),
            ..Default::default()
        };

        assert_eq!(
            classify(&entry, Some(&canonical)),
            SourceAvailability::EmbeddedSource
        );
    }

    #[test]
    fn summary_total_includes_every_category() {
        let mut summary = SourceAvailabilitySummary::default();
        for availability in [
            SourceAvailability::WorkspaceSource,
            SourceAvailability::EmbeddedSource,
            SourceAvailability::GeneratedOutline,
            SourceAvailability::MetadataOnly,
        ] {
            summary.record(availability);
        }
        assert_eq!(summary.total(), 4);
    }
}
