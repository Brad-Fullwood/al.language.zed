//! XLIFF translation support for AL projects.
//!
//! Provides XLIFF 1.2 generation from AL source (captions, tooltips, labels),
//! refresh workflow for language-specific .xlf files, and translation queries.
//!
//! # XLIFF file roles
//! - `*.g.xlf` — generated translation file (source of truth, produced by build)
//! - `*.xlf` (e.g., `de-DE.xlf`) — language-specific translation file (manually maintained)
//!
//! # Translation unit ID format
//!
//! IDs follow Microsoft's `GetLanguageSymbolId` scheme, which is what `alc`
//! writes into a `.g.xlf`: every name component is an FNV-1 hash (over the
//! name's UTF-16LE bytes) biased by `i32::MAX`, e.g.
//!
//! ```text
//! Table 3625681466 - Field 2879900210 - Property 2879900210
//! Page  3625681466 - Control 2718011747 - Property 1295455071
//! Codeunit 1535166296 - NamedType 3010734695
//! ```
//!
//! The hash is the same one `crates/al-emit/src/assemble.rs` implements and
//! verifies against `alc`, so `xlf refresh` matches IDs in an `alc`- or
//! Microsoft-produced translation file.
//!
//! **Known deviation:** `alc` folds an extension object's id-root onto the base
//! object when that base is part of the same project (emitting an
//! `al-object-target` attribute). This extractor works one file at a time and
//! has no project view, so it keeps the *declaring* object as the id root —
//! which is what `alc` also does for the dominant case of extending a
//! base-application object.

use al_syntax::IdentifierText;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use al_workspace::Workspace;

/// Upper bound on `.xlf` files we'll read into memory. Real AL translation
/// files are at most a few MB even on huge BC apps; a 64 MB cap is a
/// defence-in-depth bound that lets `parse_xliff` keep its simple
/// in-memory line-based parser without risking OOM from a malformed or
/// hostile input.
pub const MAX_XLF_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Return whether `path`'s on-disk size exceeds `MAX_XLF_FILE_BYTES`.
/// Callers should refuse to parse the file if this returns `Some(true)`.
/// `Some(false)` means the file is below the cap; `None` means metadata
/// could not be read (file missing or perms error) — caller decides
/// whether to surface that error itself.
pub fn xlf_exceeds_cap(path: &Path) -> Option<bool> {
    let meta = std::fs::metadata(path).ok()?;
    Some(meta.len() > MAX_XLF_FILE_BYTES)
}

/// A single translatable text unit extracted from AL source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationUnit {
    /// Unique ID following MS AL format: `ObjectType ObjectId - PropertyName FieldId - PropertyType`
    pub id: String,
    /// Object type (e.g. "Table", "Page", "Codeunit")
    pub object_type: String,
    /// The AL object id. Zero for a unit read back from an `.xlf`: the id
    /// carries the object's *name hash*, which no object id can be recovered
    /// from.
    pub object_id: u32,
    pub object_name: String,
    /// Source text (English caption/tooltip/label value)
    pub source: String,
    /// Translated text (if available — None means untranslated)
    pub target: Option<String>,
    pub state: TranslationState,
    /// Note (context from AL property name and field)
    pub note: Option<String>,
    /// The `Comment` the developer wrote beside the text, for translators:
    /// `Label 'Hello %1', Comment = '%1 is the customer name'`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer_note: Option<String>,
}

/// Translation state following XLIFF 1.2 conventions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationState {
    New,
    Translated,
    /// Source changed — needs review
    NeedsReviewTranslation,
    /// Source was removed — no longer used
    Final,
}

impl TranslationState {
    fn as_xliff_state(&self) -> &'static str {
        match self {
            TranslationState::New => "new",
            TranslationState::Translated => "translated",
            TranslationState::NeedsReviewTranslation => "needs-review-translation",
            TranslationState::Final => "final",
        }
    }

    fn from_xliff_state(s: &str) -> Self {
        match s {
            "translated" => TranslationState::Translated,
            "needs-review-translation" => TranslationState::NeedsReviewTranslation,
            "final" => TranslationState::Final,
            _ => TranslationState::New,
        }
    }
}

mod extract;
mod format;
mod refresh;
mod suggest;
#[cfg(test)]
mod tests;

pub use extract::*;
pub use format::*;
pub use refresh::*;
pub use suggest::*;

/// Generate the `.g.xlf` file for a workspace and write it to the Translations directory.
///
/// Creates `<project_root>/Translations/<AppName>.g.xlf`.
/// Returns:
/// - `Ok(Some((path, count)))` on success.
/// - `Ok(None)` when no translatable units exist (not an error).
/// - `Err(io::Error)` when directory creation or file write fails.
pub fn build_xliff(
    workspace: &Workspace,
    project_root: &Path,
) -> std::io::Result<Option<(PathBuf, usize)>> {
    let app_name = read_app_name(project_root)?;

    let units = extract_translation_units(workspace);
    if units.is_empty() {
        return Ok(None);
    }

    let xlf_content = generate_xliff(&app_name, "en-US", "en-US", &units);

    let translations_dir = project_root.join("Translations");
    std::fs::create_dir_all(&translations_dir)?;

    let xlf_path = translations_dir.join(format!("{}.g.xlf", app_name));
    std::fs::write(&xlf_path, xlf_content)?;

    Ok(Some((xlf_path, units.len())))
}

pub(super) fn read_app_name(project_root: &Path) -> std::io::Result<String> {
    let manifest = al_project::project::load_app_manifest(project_root)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let name = al_types::sanitize_filename_component(&manifest.name.replace([' ', '"', '\''], ""));
    if name == "_" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} must contain a non-empty application name",
                project_root.join("app.json").display()
            ),
        ));
    }
    Ok(name)
}
