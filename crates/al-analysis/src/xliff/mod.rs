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
//! **Extension objects:** `alc` roots an extension's ids on the object it
//! extends when the project declares that object, otherwise on the extension
//! with the lowest object id for that target, and writes an `al-object-target`
//! attribute. `extract` reads every object header first to do the same, see its
//! module comment for the decompiled alc methods.

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
    /// alc's `al-object-target` attribute: the symbol an extension unit
    /// belongs to. `None` for objects that are not extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_target: Option<String>,
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

    let options = XliffOptions::from_features(&read_app_features(project_root));
    let units = extract_translation_units_with(workspace, options);
    if units.is_empty() {
        return Ok(None);
    }

    let xlf_content = generate_xliff(&app_name, "en-US", "en-US", &units);

    let translations_dir = translations_dir_inside(project_root)?;
    let xlf_path = translations_dir.join(generated_xlf_file_name(&app_name));
    // A fresh temporary file renamed over the target replaces a symlink
    // planted at `xlf_path` rather than writing through it. `std::fs::write`
    // followed the link, dangling or not, to wherever the repository aimed it.
    let mut temp = tempfile::Builder::new()
        .prefix(".al-xlf-")
        .suffix(".tmp")
        .tempfile_in(&translations_dir)?;
    std::io::Write::write_all(&mut temp, xlf_content.as_bytes())?;
    temp.persist(&xlf_path).map_err(|error| error.error)?;

    Ok(Some((xlf_path, units.len())))
}

/// `<project_root>/Translations`, created when missing, refused when it
/// resolves outside the project.
///
/// A clone can ship `Translations` as a symlink to any directory, and the
/// generated file carries caption and label text from the repository's source.
fn translations_dir_inside(project_root: &Path) -> std::io::Result<PathBuf> {
    let translations_dir = project_root.join("Translations");
    if std::fs::symlink_metadata(&translations_dir).is_err() {
        std::fs::create_dir(&translations_dir)?;
    }
    let root = project_root.canonicalize()?;
    let resolved = translations_dir.canonicalize()?;
    if !resolved.starts_with(&root) || !resolved.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} resolves outside the project, so the translation file is not written there",
                translations_dir.display()
            ),
        ));
    }
    Ok(resolved)
}

/// The `features` array of app.json. `AppManifest` does not carry it and the
/// array is optional, so a missing or unreadable file means no features.
fn read_app_features(project_root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(project_root.join("app.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
    else {
        return Vec::new();
    };
    json.get("features")
        .and_then(|features| features.as_array())
        .map(|features| {
            features
                .iter()
                .filter_map(|feature| feature.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The application name from app.json, as it goes into `original=`.
pub(super) fn read_app_name(project_root: &Path) -> std::io::Result<String> {
    let manifest = al_project::project::load_app_manifest(project_root)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if manifest.name.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} must contain a non-empty application name",
                project_root.join("app.json").display()
            ),
        ));
    }
    Ok(manifest.name)
}

/// The file name alc gives the generated file, `<app name>.g.xlf`.
///
/// alc 18.1 keeps the spaces: a project named "AL Zed Journey" gets `AL Zed Journey.g.xlf`
/// (journey stage 07, alc output). alc has no file name rule for invalid characters because
/// it rejects such an app name first (AL1053 on `A:B?C*D.`, checked with alc 18.1.43). The
/// removal below only keeps one path component for a name alc would reject.
pub fn generated_xlf_file_name(app_name: &str) -> String {
    let cleaned: String = app_name
        .chars()
        .filter(|c| {
            !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && !c.is_control()
        })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    let stem = if cleaned.is_empty() {
        "default"
    } else {
        cleaned
    };
    format!("{stem}.g.xlf")
}

/// The generated file name for the project in `project_root`, from its app.json.
pub fn project_generated_xlf_name(project_root: &Path) -> Option<String> {
    read_app_name(project_root)
        .ok()
        .map(|name| generated_xlf_file_name(&name))
}
