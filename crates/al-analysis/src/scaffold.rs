//! AL project scaffolding.
//!
//! Creates new AL projects from templates with standard file structure:
//! - `app.json` — project manifest
//! - `.gitignore` — AL-specific ignores
//! - `.zed/debug.json` — debug/launch configuration
//! - `src/` — source directory

use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Comma-separated list of the built-in template names, for error messages.
const BUILTIN_TEMPLATE_NAMES: &str = "default, pte, appsource, library, test, copilot, agent, api";

/// Project template type for scaffolding.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProjectTemplate {
    #[default]
    Default,
    PerTenantExtension,
    AppSourceApp,
    /// Library/dependency with no UI
    Library,
    TestApp,
    /// Copilot AI extension (chat participant + completions)
    Copilot,
    /// Agent extension (background job + AI orchestration)
    Agent,
    /// API-only extension (REST API pages)
    Api,
    /// A user-defined template resolved from the templates directory
    /// (`$AL_TEMPLATES_DIR` or `~/.config/al/templates/<name>/`). Carries the
    /// resolved on-disk location + parsed descriptor so [`create_project`] can
    /// materialize it without re-reading the descriptor.
    Custom(CustomTemplate),
}

/// A resolved user-defined template: its name, the directory it lives in, and
/// its parsed `template.json` descriptor. Produced by [`resolve_custom_template`]
/// (and by [`ProjectTemplate::from_str`] when a name is not a built-in).
#[derive(Debug, Clone, PartialEq)]
pub struct CustomTemplate {
    /// The template name as requested (a single, validated path component).
    pub name: String,
    /// Absolute path to the template directory (`<templates_root>/<name>`).
    pub dir: PathBuf,
    /// Parsed `template.json` descriptor.
    pub descriptor: TemplateDescriptor,
}

/// The `template.json` descriptor for a user-defined template.
///
/// All fields are optional. Schema (camelCase JSON):
/// - `description`: free-text, informational only.
/// - `generateId` (bool, default `false`): when `true`, the `{{id}}` placeholder
///   is replaced with a freshly generated v4 GUID instead of the config id.
/// - `idFrom` / `idTo` (u32): drive the `{{id_from}}` / `{{id_to}}` placeholders.
///   `idFrom` defaults to 50100; `idTo` defaults to `idFrom + 49`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateDescriptor {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub generate_id: bool,
    #[serde(default)]
    pub id_from: Option<u32>,
    #[serde(default)]
    pub id_to: Option<u32>,
}

impl FromStr for ProjectTemplate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "default" | "extension" => Ok(Self::Default),
            "pte" | "pertenantextension" => Ok(Self::PerTenantExtension),
            "appsource" | "appsourceapp" => Ok(Self::AppSourceApp),
            "library" | "lib" => Ok(Self::Library),
            "test" | "testapp" => Ok(Self::TestApp),
            "copilot" => Ok(Self::Copilot),
            "agent" => Ok(Self::Agent),
            "api" => Ok(Self::Api),
            // Not a built-in: fall back to resolving a user-defined template of
            // this name from the templates directory. A bad name (path
            // traversal) or a malformed descriptor is a hard error; a name that
            // simply has no matching directory is reported as "unknown".
            _ => match resolve_custom_template(s)? {
                Some(custom) => Ok(Self::Custom(custom)),
                None => {
                    let root = templates_root()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<no templates directory>".to_string());
                    Err(format!(
                        "Unknown template '{s}'. Built-ins: {BUILTIN_TEMPLATE_NAMES}. \
                         No custom template named '{s}' was found in {root}."
                    ))
                }
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    pub name: String,
    pub publisher: String,
    pub id: String,
    pub version: String,
    pub runtime: String,
    pub target: String,
    pub template: ProjectTemplate,
}

impl Default for ScaffoldConfig {
    fn default() -> Self {
        Self {
            name: "MyApp".to_string(),
            publisher: "Default Publisher".to_string(),
            id: "00000000-0000-0000-0000-000000000000".to_string(),
            version: "1.0.0.0".to_string(),
            runtime: "14.0".to_string(),
            target: "Cloud".to_string(),
            template: ProjectTemplate::Default,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaffoldResult {
    pub project_dir: String,
    pub files_created: Vec<String>,
}

pub fn create_project(dir: &Path, config: &ScaffoldConfig) -> Result<ScaffoldResult, String> {
    if dir.join("app.json").exists() {
        return Err(format!(
            "Directory already contains an AL project: {}",
            dir.display()
        ));
    }

    // User-defined templates own their entire file tree (including app.json),
    // so they are materialized directly rather than going through the built-in
    // app.json/.gitignore/debug.json/starter flow below.
    if let ProjectTemplate::Custom(custom) = &config.template {
        return materialize_custom_template(dir, custom, config);
    }

    std::fs::create_dir_all(dir.join("src"))
        .map_err(|e| format!("Failed to create project directory: {e}"))?;
    std::fs::create_dir_all(dir.join(".zed"))
        .map_err(|e| format!("Failed to create .zed directory: {e}"))?;

    let mut files = Vec::new();

    // app.json — propagate serialization error rather than silently writing empty file
    let app_json = generate_app_json(config)?;
    atomic_write(&dir.join("app.json"), app_json.as_bytes(), "app.json")?;
    files.push("app.json".to_string());

    let gitignore = generate_gitignore();
    atomic_write(&dir.join(".gitignore"), gitignore.as_bytes(), ".gitignore")?;
    files.push(".gitignore".to_string());

    // .zed/debug.json — propagate serialization error rather than silently writing empty file
    let debug_json = generate_debug_json()?;
    atomic_write(
        &dir.join(".zed/debug.json"),
        debug_json.as_bytes(),
        ".zed/debug.json",
    )?;
    files.push(".zed/debug.json".to_string());

    let template_files = generate_template_files(dir, config)?;
    files.extend(template_files);

    Ok(ScaffoldResult {
        project_dir: dir.display().to_string(),
        files_created: files,
    })
}

/// Write `content` to `path` atomically.
///
/// Writes to a sibling `<file>.<pid>.tmp` then `rename(2)`s into place. On
/// POSIX, rename is atomic when source and destination are on the same
/// filesystem (always the case here — the temp is in the same directory).
/// Replaces `std::fs::write(path, content)` calls that would otherwise
/// leave a half-written `.al` file on disk after a crash / signal.
/// F-OPEN-034.
fn atomic_write(path: &Path, content: &[u8], label: &str) -> Result<(), String> {
    use std::io::Write;

    let pid = std::process::id();
    let tmp_path = match path.file_name() {
        Some(n) => path.with_file_name(format!("{}.{pid}.tmp", n.to_string_lossy())),
        None => return Err(format!("Failed to derive tempfile name for {label}")),
    };

    let mut file = std::fs::File::create(&tmp_path)
        .map_err(|e| format!("Failed to open tempfile for {label}: {e}"))?;
    if let Err(e) = file.write_all(content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("Failed to write {label}: {e}"));
    }
    if let Err(e) = file.sync_all() {
        // sync_all failing is non-fatal for correctness — rename is still
        // atomic, durability after a power loss is the only loss. Log via
        // tracing so an op can see it, but don't fail the scaffold.
        tracing::warn!(label, error = %e, "scaffold: sync_all on tempfile failed");
    }
    drop(file);

    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("Failed to rename tempfile for {label}: {e}")
    })
}

fn generate_template_files(dir: &Path, config: &ScaffoldConfig) -> Result<Vec<String>, String> {
    match &config.template {
        ProjectTemplate::Default | ProjectTemplate::PerTenantExtension => {
            let starter = generate_starter_codeunit(config);
            let name = "src/HelloWorld.Codeunit.al";
            atomic_write(&dir.join(name), starter.as_bytes(), "starter codeunit")?;
            Ok(vec![name.to_string()])
        }
        ProjectTemplate::AppSourceApp => {
            let starter = generate_starter_codeunit(config);
            let name = "src/HelloWorld.Codeunit.al";
            atomic_write(&dir.join(name), starter.as_bytes(), "starter codeunit")?;
            let cop = generate_app_source_cop_json()?;
            let cop_name = "AppSourceCop.json";
            atomic_write(&dir.join(cop_name), cop.as_bytes(), "AppSourceCop.json")?;
            Ok(vec![name.to_string(), cop_name.to_string()])
        }
        ProjectTemplate::Library => {
            let lib = generate_library_codeunit(config);
            let name = "src/Library.Codeunit.al";
            atomic_write(&dir.join(name), lib.as_bytes(), "library codeunit")?;
            Ok(vec![name.to_string()])
        }
        ProjectTemplate::TestApp => {
            let test = generate_test_codeunit(config);
            let name = "src/Test.Codeunit.al";
            atomic_write(&dir.join(name), test.as_bytes(), "test codeunit")?;
            Ok(vec![name.to_string()])
        }
        ProjectTemplate::Copilot => {
            let participant = generate_copilot_codeunit(config);
            let part_name = "src/CopilotParticipant.Codeunit.al";
            atomic_write(
                &dir.join(part_name),
                participant.as_bytes(),
                "copilot participant",
            )?;
            let openai = generate_azure_openai_codeunit(config);
            let ai_name = "src/AzureOpenAI.Codeunit.al";
            atomic_write(
                &dir.join(ai_name),
                openai.as_bytes(),
                "Azure OpenAI codeunit",
            )?;
            Ok(vec![part_name.to_string(), ai_name.to_string()])
        }
        ProjectTemplate::Agent => {
            let agent = generate_agent_codeunit(config);
            let agent_name = "src/Agent.Codeunit.al";
            atomic_write(&dir.join(agent_name), agent.as_bytes(), "agent codeunit")?;
            let handler = generate_agent_job_handler(config);
            let handler_name = "src/AgentJobHandler.Codeunit.al";
            atomic_write(
                &dir.join(handler_name),
                handler.as_bytes(),
                "agent job handler",
            )?;
            Ok(vec![agent_name.to_string(), handler_name.to_string()])
        }
        ProjectTemplate::Api => {
            let api = generate_api_page(config);
            let name = "src/Api.Page.al";
            atomic_write(&dir.join(name), api.as_bytes(), "API page")?;
            Ok(vec![name.to_string()])
        }
        // Custom templates are intercepted in `create_project` and never reach
        // here. This arm keeps the match exhaustive; reaching it would mean a
        // future refactor routed a custom template through the built-in flow,
        // which would silently drop its files — fail loudly instead.
        ProjectTemplate::Custom(_) => {
            Err("internal error: custom template reached generate_template_files".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// User-defined (custom) templates — gap C12.
//
// A custom template lives at `<templates_root>/<name>/` and contains:
//   - `template.json` — a [`TemplateDescriptor`].
//   - `files/`        — the project tree, copied verbatim into the new project
//                       with `{{placeholder}}` substitution in file contents
//                       AND in file/directory names.
//
// `templates_root` is, in order of precedence:
//   1. `$AL_TEMPLATES_DIR`
//   2. `$XDG_CONFIG_HOME/al/templates`
//   3. `$HOME/.config/al/templates`
// ---------------------------------------------------------------------------

/// Resolve the templates root directory from the environment, if any.
fn templates_root() -> Option<PathBuf> {
    let non_empty = |v: std::ffi::OsString| (!v.is_empty()).then_some(v);
    if let Some(dir) = std::env::var_os("AL_TEMPLATES_DIR").and_then(non_empty) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").and_then(non_empty) {
        return Some(PathBuf::from(xdg).join("al").join("templates"));
    }
    if let Some(home) = std::env::var_os("HOME").and_then(non_empty) {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("al")
                .join("templates"),
        );
    }
    None
}

/// A template name must be a single, normal path component — no `..`, no path
/// separators, no absolute prefix. This is the first line of defence against
/// path traversal via the requested template name (e.g. `../../etc`).
fn valid_template_name(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') {
        return false;
    }
    let path = Path::new(name);
    let mut comps = path.components();
    matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(_)), None)
    )
}

fn invalid_name_msg(name: &str) -> String {
    format!(
        "Invalid template name '{name}': a template name must be a single path \
         component without '..' or path separators."
    )
}

/// Resolve a user-defined template by name from the environment-configured
/// templates root.
///
/// - `Ok(Some(_))` — a valid template directory with a parseable descriptor.
/// - `Ok(None)`    — no templates root is configured, or no directory of this
///                   name exists under it (reported upstream as "unknown").
/// - `Err(_)`      — the name is unsafe, or the descriptor is missing/malformed.
fn resolve_custom_template(name: &str) -> Result<Option<CustomTemplate>, String> {
    if !valid_template_name(name) {
        return Err(invalid_name_msg(name));
    }
    match templates_root() {
        Some(root) => resolve_custom_template_in(&root, name),
        None => Ok(None),
    }
}

/// Resolve a custom template under an explicit `root` (testable without the
/// process environment). Assumes `name` validity is the caller's concern but
/// re-checks it defensively.
fn resolve_custom_template_in(root: &Path, name: &str) -> Result<Option<CustomTemplate>, String> {
    if !valid_template_name(name) {
        return Err(invalid_name_msg(name));
    }
    let template_dir = root.join(name);
    if !template_dir.is_dir() {
        return Ok(None);
    }
    let descriptor_path = template_dir.join("template.json");
    if !descriptor_path.is_file() {
        return Err(format!(
            "Custom template '{name}' at {} is missing template.json.",
            template_dir.display()
        ));
    }
    let raw = std::fs::read_to_string(&descriptor_path).map_err(|e| {
        format!(
            "Failed to read template descriptor {}: {e}",
            descriptor_path.display()
        )
    })?;
    let descriptor: TemplateDescriptor = serde_json::from_str(&raw)
        .map_err(|e| format!("Invalid template.json for custom template '{name}': {e}"))?;
    Ok(Some(CustomTemplate {
        name: name.to_string(),
        dir: template_dir,
        descriptor,
    }))
}

/// Materialize a resolved custom template into `dir`, applying placeholder
/// substitution to file contents and to file/directory names.
fn materialize_custom_template(
    dir: &Path,
    custom: &CustomTemplate,
    config: &ScaffoldConfig,
) -> Result<ScaffoldResult, String> {
    let files_root = custom.dir.join("files");
    if !files_root.is_dir() {
        return Err(format!(
            "Custom template '{}' has no 'files/' directory at {}.",
            custom.name,
            files_root.display()
        ));
    }

    // Compute placeholder values once.
    let app_id = if custom.descriptor.generate_id {
        fresh_guid()
    } else {
        config.id.clone()
    };
    let id_from = custom.descriptor.id_from.unwrap_or(50100);
    let id_to = custom
        .descriptor
        .id_to
        .unwrap_or_else(|| id_from.saturating_add(49));
    let substitutions: Vec<(String, String)> = vec![
        ("{{name}}".to_string(), config.name.clone()),
        ("{{publisher}}".to_string(), config.publisher.clone()),
        ("{{version}}".to_string(), config.version.clone()),
        ("{{runtime}}".to_string(), config.runtime.clone()),
        ("{{target}}".to_string(), config.target.clone()),
        ("{{id}}".to_string(), app_id),
        ("{{id_from}}".to_string(), id_from.to_string()),
        ("{{id_to}}".to_string(), id_to.to_string()),
    ];
    let substitute = |input: &str| -> String {
        let mut out = input.to_string();
        for (needle, value) in &substitutions {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), value);
            }
        }
        out
    };

    // Gather the template's files (relative to `files/`), rejecting symlinks
    // and any path that escapes the root.
    let mut rel_files: Vec<PathBuf> = Vec::new();
    collect_template_files(&files_root, &files_root, &mut rel_files)?;
    rel_files.sort();

    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create project directory: {e}"))?;

    let mut created = Vec::new();
    for rel in &rel_files {
        // Defence in depth: the source path must be a normal relative path.
        guard_relative(rel)?;
        // Substitute placeholders in path components, then re-check — a
        // substituted value must not be able to inject `..` or an absolute
        // escape into the destination path.
        let dest_rel = substitute_path(rel, &substitute)?;
        guard_relative(&dest_rel)?;

        let src_path = files_root.join(rel);
        let dest_path = dir.join(&dest_rel);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory {}: {e}", parent.display()))?;
        }

        // Substitute placeholders in UTF-8 contents; copy non-UTF-8 (binary)
        // assets verbatim.
        let bytes = std::fs::read(&src_path)
            .map_err(|e| format!("Failed to read template file {}: {e}", src_path.display()))?;
        let out_bytes = match String::from_utf8(bytes) {
            Ok(text) => substitute(&text).into_bytes(),
            Err(err) => err.into_bytes(),
        };
        let rel_display = dest_rel.to_string_lossy().replace('\\', "/");
        atomic_write(&dest_path, &out_bytes, &rel_display)?;
        created.push(rel_display);
    }

    if created.is_empty() {
        return Err(format!(
            "Custom template '{}' contains no files under {}.",
            custom.name,
            files_root.display()
        ));
    }
    created.sort();

    Ok(ScaffoldResult {
        project_dir: dir.display().to_string(),
        files_created: created,
    })
}

/// Recursively collect regular files under `dir`, pushing their paths relative
/// to `root`. Symlinks are rejected (a traversal vector); directory entries are
/// recursed into. Uses `DirEntry::file_type`, which does not traverse symlinks.
fn collect_template_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read template directory {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read template entry: {e}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "Refusing to materialize symlink in template (path traversal risk): {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            collect_template_files(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| format!("Template path escaped root: {}", path.display()))?;
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

/// Reject any relative path that contains a `..` component or an absolute
/// prefix/root. Mirrors the daemon's absolute-path guard for project dirs.
fn guard_relative(rel: &Path) -> Result<(), String> {
    if rel.as_os_str().is_empty() {
        return Err("empty template file path".to_string());
    }
    for comp in rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "path traversal ('..') is not allowed in template path: {}",
                    rel.display()
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "absolute paths are not allowed in template path: {}",
                    rel.display()
                ));
            }
        }
    }
    Ok(())
}

/// Apply placeholder substitution to each `Normal` component of a relative path.
fn substitute_path(rel: &Path, substitute: &impl Fn(&str) -> String) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(os) => {
                let s = os.to_str().ok_or_else(|| {
                    format!("non-UTF-8 path component in template: {}", rel.display())
                })?;
                out.push(substitute(s));
            }
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "unexpected path component in template: {}",
                    rel.display()
                ))
            }
        }
    }
    Ok(out)
}

/// Generate a fresh v4-shaped GUID (canonical lowercase, unbraced) for the
/// `{{id}}` placeholder when a descriptor requests `generateId`.
///
/// al-analysis has no RNG dependency, so this seeds a SplitMix64 stream from the
/// wall clock, the process id, and a monotonic counter. That is *not*
/// cryptographic randomness, but a scaffold's app id only needs to be unique,
/// which this comfortably provides (distinct calls advance the counter).
fn fresh_guid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seed = nanos
        ^ ((std::process::id() as u64) << 32)
        ^ COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);

    let hi = splitmix64(seed);
    let lo = splitmix64(seed ^ 0xD1B5_4A32_D192_ED03);
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&hi.to_le_bytes());
    b[8..].copy_from_slice(&lo.to_le_bytes());
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 1 (RFC 4122)

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14],
        b[15]
    )
}

fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn generate_app_json(config: &ScaffoldConfig) -> Result<String, String> {
    let (target, features, analyzers) = match &config.template {
        ProjectTemplate::AppSourceApp => (
            "Cloud",
            serde_json::json!(["NoImplicitWith", "GenerateCaptions"]),
            serde_json::json!(["AppSourceCop", "PerTenantExtensionCop", "UICop"]),
        ),
        ProjectTemplate::Api => (
            "Cloud",
            serde_json::json!(["NoImplicitWith"]),
            serde_json::json!(["PerTenantExtensionCop"]),
        ),
        _ => (
            config.target.as_str(),
            serde_json::json!(["NoImplicitWith"]),
            serde_json::json!(["PerTenantExtensionCop"]),
        ),
    };

    let mut manifest = serde_json::json!({
        "id": config.id,
        "name": config.name,
        "publisher": config.publisher,
        "version": config.version,
        "brief": "",
        "description": "",
        "privacyStatement": "",
        "EULA": "",
        "help": "",
        "url": "",
        "logo": "",
        "dependencies": [],
        "screenshots": [],
        "platform": "1.0.0.0",
        "application": "26.0.0.0",
        "idRanges": [{"from": 50100, "to": 50149}],
        "resourceExposurePolicy": {
            "allowDebugging": true,
            "allowDownloadingSource": true,
            "includeSourceInSymbolFile": true
        },
        "runtime": config.runtime,
        "target": target,
        "features": features,
        "codeAnalyzers": analyzers
    });

    if matches!(
        &config.template,
        ProjectTemplate::Copilot | ProjectTemplate::Agent
    ) {
        manifest["capabilities"] = serde_json::json!(["AzureOpenAI"]);
    }

    serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize app.json: {e}"))
}

fn generate_gitignore() -> String {
    "\
# AL build artifacts
*.app
*.dep
*.xlf~

# Package cache
.alpackages/

# VS Code / Zed settings (keep debug.json)
.vscode/settings.json

# OS files
.DS_Store
Thumbs.db
"
    .to_string()
}

fn generate_debug_json() -> Result<String, String> {
    serde_json::to_string_pretty(&serde_json::json!([
        {
            "adapter": "al",
            "label": "Publish: Your own server",
            "request": "launch",
            "environmentType": "OnPrem",
            "server": "http://bcserver",
            "serverInstance": "BC",
            "authentication": "UserPassword",
            "startupObjectId": 22,
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "launchBrowser": true,
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "tenant": "default",
            "usePublicURLFromServer": true,
            "build": {"command": "al", "args": ["compile"]}
        },
        {
            "adapter": "al",
            "label": "Publish: Cloud Sandbox",
            "request": "launch",
            "environmentType": "Sandbox",
            "environmentName": "sandbox",
            "startupObjectId": 22,
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "launchBrowser": true,
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "build": {"command": "al", "args": ["compile"]}
        },
        {
            "adapter": "al",
            "label": "Attach: Your own server",
            "request": "attach",
            "environmentType": "OnPrem",
            "server": "http://bcserver",
            "serverInstance": "BC",
            "authentication": "UserPassword",
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "breakOnNext": "WebServiceClient",
            "tenant": "default"
        },
        {
            "adapter": "al",
            "label": "Attach: Cloud Sandbox",
            "request": "attach",
            "environmentType": "Sandbox",
            "environmentName": "sandbox",
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "breakOnNext": "WebServiceClient"
        }
    ]))
    .map_err(|e| format!("Failed to serialize debug.json: {e}"))
}

fn generate_starter_codeunit(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"codeunit 50100 "Hello World"
{{
    trigger OnRun()
    begin
        Message('Hello from {name}!');
    end;
}}
"#
    )
}

fn generate_library_codeunit(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"codeunit 50100 "{name} Library"
{{
    procedure GetVersion(): Text
    begin
        exit('1.0.0.0');
    end;
}}
"#
    )
}

fn generate_test_codeunit(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"codeunit 50100 "{name} Test"
{{
    Subtype = Test;

    [Test]
    procedure TestSomething()
    begin
        // Arrange

        // Act

        // Assert
        Assert.IsTrue(true, 'Placeholder test');
    end;

    var
        Assert: Codeunit "Library Assert";
}}
"#
    )
}

fn generate_copilot_codeunit(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"codeunit 50100 "{name} Copilot Participant"
{{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Copilot Chat", 'OnGenerateCompletion', '', false, false)]
    local procedure OnGenerateCompletion(var Prompt: Text; var Completion: Text)
    var
        AzureOpenAI: Codeunit "Azure OpenAI";
    begin
        AzureOpenAI.SetAuthorization(Enum::"AOAI Model Type"::"Chat Completions", GetEndpoint(), GetDeployment(), GetApiKey());
        AzureOpenAI.GenerateTextCompletion(Prompt, Completion);
    end;

    local procedure GetEndpoint(): Text
    begin
        exit('');
    end;

    local procedure GetDeployment(): Text
    begin
        exit('gpt-4o');
    end;

    local procedure GetApiKey(): SecretText
    var
        Key: SecretText;
    begin
        exit(Key);
    end;
}}
"#
    )
}

fn generate_azure_openai_codeunit(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    // The second interpolation is inside a single-quoted AL string literal;
    // AL escapes single quotes as `''` (not `\'`). Apply the same convention.
    let single_quoted = config.name.replace('\'', "''");
    format!(
        r#"codeunit 50101 "{name} Azure OpenAI Helper"
{{
    procedure BuildPrompt(UserQuery: Text): Text
    begin
        exit(StrSubstNo('You are a helpful assistant for %1. %2', '{single_quoted}', UserQuery));
    end;
}}
"#
    )
}

fn generate_agent_codeunit(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"codeunit 50100 "{name} Agent"
{{
    procedure Run(Instructions: Text): Text
    var
        JobHandler: Codeunit "{name} Agent Job Handler";
        Result: Text;
    begin
        JobHandler.Execute(Instructions, Result);
        exit(Result);
    end;
}}
"#
    )
}

fn generate_agent_job_handler(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"codeunit 50101 "{name} Agent Job Handler"
{{
    procedure Execute(Instructions: Text; var Result: Text)
    begin
        // Call your AI endpoint here, e.g. via HttpClient to Azure OpenAI
        // and store the response text in Result.
        Result := StrSubstNo('Processed: %1', Instructions);
    end;
}}
"#
    )
}

fn generate_api_page(config: &ScaffoldConfig) -> String {
    let name = crate::permissions::al_escape_name(&config.name);
    format!(
        r#"page 50100 "{name} API"
{{
    PageType = API;
    APIPublisher = 'defaultPublisher';
    APIGroup = 'defaultGroup';
    APIVersion = 'v1.0';
    EntityName = 'item';
    EntitySetName = 'items';
    SourceTable = Customer;
    DelayedInsert = true;

    layout
    {{
        area(Content)
        {{
            repeater(Group)
            {{
                field(id; Rec.SystemId)
                {{
                    Caption = 'ID';
                    ApplicationArea = All;
                }}
                field(no; Rec."No.")
                {{
                    Caption = 'No';
                    ApplicationArea = All;
                }}
                field(name; Rec.Name)
                {{
                    Caption = 'Name';
                    ApplicationArea = All;
                }}
            }}
        }}
    }}
}}
"#
    )
}

fn generate_app_source_cop_json() -> Result<String, String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "mandatoryAffixes": [],
        "mandatoryAnalyzers": ["AppSourceCop", "PerTenantExtensionCop", "UICop"],
        "obsoleteTagMinAllowedMajorMinor": "14.0"
    }))
    .map_err(|e| format!("Failed to serialize AppSourceCop.json: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_creates_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig::default();
        let result = create_project(dir.path(), &config).unwrap();

        assert_eq!(result.files_created.len(), 4);
        assert!(dir.path().join("app.json").exists());
        assert!(dir.path().join(".gitignore").exists());
        assert!(dir.path().join(".zed/debug.json").exists());
        assert!(dir.path().join("src/HelloWorld.Codeunit.al").exists());
    }

    #[test]
    fn scaffold_refuses_existing_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.json"), "{}").unwrap();

        let config = ScaffoldConfig::default();
        let result = create_project(dir.path(), &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already contains"));
    }

    #[test]
    fn app_json_has_required_fields() {
        let config = ScaffoldConfig {
            name: "Test App".to_string(),
            publisher: "Test Publisher".to_string(),
            ..Default::default()
        };
        let json = generate_app_json(&config).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "Test App");
        assert_eq!(parsed["publisher"], "Test Publisher");
        assert!(parsed["idRanges"].is_array());
        assert!(parsed["dependencies"].is_array());
    }

    #[test]
    fn gitignore_excludes_app_files() {
        let content = generate_gitignore();
        assert!(content.contains("*.app"));
        assert!(content.contains(".alpackages/"));
    }

    #[test]
    fn starter_codeunit_includes_project_name() {
        let config = ScaffoldConfig {
            name: "My Cool App".to_string(),
            ..Default::default()
        };
        let content = generate_starter_codeunit(&config);
        assert!(content.contains("My Cool App"));
        assert!(content.contains("codeunit 50100"));
    }

    #[test]
    fn scaffold_creates_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("new_project");
        let config = ScaffoldConfig::default();
        let result = create_project(&subdir, &config);
        assert!(result.is_ok());
        assert!(subdir.join("src").is_dir());
        assert!(subdir.join(".zed").is_dir());
    }

    #[test]
    fn scaffold_result_serializes() {
        let result = ScaffoldResult {
            project_dir: "/tmp/test".to_string(),
            files_created: vec!["app.json".to_string()],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"projectDir\""));
        assert!(json.contains("\"filesCreated\""));
    }

    #[test]
    fn generate_app_json_returns_valid_json() {
        let config = ScaffoldConfig::default();
        let json = generate_app_json(&config).expect("serialization should not fail");
        let _: serde_json::Value = serde_json::from_str(&json).expect("should be valid JSON");
    }

    #[test]
    fn generate_debug_json_returns_valid_json() {
        let json = generate_debug_json().expect("serialization should not fail");
        let _: serde_json::Value = serde_json::from_str(&json).expect("should be valid JSON");
    }

    #[test]
    fn template_fromstr_parses_all_variants() {
        assert!(matches!(
            "default".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::Default
        ));
        assert!(matches!(
            "copilot".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::Copilot
        ));
        assert!(matches!(
            "agent".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::Agent
        ));
        assert!(matches!(
            "api".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::Api
        ));
        assert!(matches!(
            "pte".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::PerTenantExtension
        ));
        assert!(matches!(
            "appsource".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::AppSourceApp
        ));
        assert!(matches!(
            "library".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::Library
        ));
        assert!(matches!(
            "test".parse::<ProjectTemplate>().unwrap(),
            ProjectTemplate::TestApp
        ));
        assert!("unknown".parse::<ProjectTemplate>().is_err());
    }

    #[test]
    fn scaffold_copilot_template_creates_ai_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            name: "MyCopilot".to_string(),
            template: ProjectTemplate::Copilot,
            ..Default::default()
        };
        let result = create_project(dir.path(), &config).unwrap();
        assert!(result
            .files_created
            .iter()
            .any(|f| f.contains("CopilotParticipant")));
        assert!(result
            .files_created
            .iter()
            .any(|f| f.contains("AzureOpenAI")));
        let participant =
            std::fs::read_to_string(dir.path().join("src/CopilotParticipant.Codeunit.al")).unwrap();
        assert!(participant.contains("EventSubscriber"));
        assert!(participant.contains("OnGenerateCompletion"));
    }

    #[test]
    fn scaffold_agent_template_creates_job_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            name: "MyAgent".to_string(),
            template: ProjectTemplate::Agent,
            ..Default::default()
        };
        let result = create_project(dir.path(), &config).unwrap();
        assert!(result
            .files_created
            .iter()
            .any(|f| f.contains("Agent.Codeunit")));
        assert!(result
            .files_created
            .iter()
            .any(|f| f.contains("AgentJobHandler")));
    }

    #[test]
    fn scaffold_api_template_creates_api_page() {
        let dir = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            name: "MyApi".to_string(),
            template: ProjectTemplate::Api,
            ..Default::default()
        };
        let result = create_project(dir.path(), &config).unwrap();
        assert!(result.files_created.iter().any(|f| f.contains("Api.Page")));
        let page = std::fs::read_to_string(dir.path().join("src/Api.Page.al")).unwrap();
        assert!(page.contains("PageType = API"));
        assert!(page.contains("APIVersion"));
    }

    #[test]
    fn copilot_app_json_has_capability() {
        let config = ScaffoldConfig {
            template: ProjectTemplate::Copilot,
            ..Default::default()
        };
        let json = generate_app_json(&config).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["capabilities"].is_array());
    }

    #[test]
    fn appsource_scaffold_creates_cop_json() {
        let dir = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            template: ProjectTemplate::AppSourceApp,
            ..Default::default()
        };
        let result = create_project(dir.path(), &config).unwrap();
        assert!(result
            .files_created
            .iter()
            .any(|f| f == "AppSourceCop.json"));
    }

    #[test]
    fn atomic_write_does_not_leave_tempfile_on_success() {
        // Regression for F-OPEN-034: a successful scaffold must not leave
        // `<file>.<pid>.tmp` lingering next to the final artefact. The
        // helper renames atomically, so the tempfile name should not
        // exist after the call returns.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("foo.al");
        atomic_write(&target, b"hello", "foo.al").unwrap();
        assert!(target.exists(), "final file must exist after atomic_write");
        let pid = std::process::id();
        let tmp = dir.path().join(format!("foo.al.{pid}.tmp"));
        assert!(
            !tmp.exists(),
            "tempfile {tmp:?} should have been renamed away"
        );
        let read = std::fs::read(&target).unwrap();
        assert_eq!(read, b"hello");
    }

    #[test]
    fn atomic_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("foo.al");
        atomic_write(&target, b"v1", "foo.al").unwrap();
        atomic_write(&target, b"v2", "foo.al").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
    }

    #[test]
    fn atomic_write_returns_err_on_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("no-such-subdir/foo.al");
        let result = atomic_write(&target, b"hello", "foo.al");
        assert!(result.is_err(), "expected Err for missing parent dir");
        let pid = std::process::id();
        let tmp = dir.path().join(format!("no-such-subdir/foo.al.{pid}.tmp"));
        assert!(!tmp.exists());
    }

    /// Asserts that the given AL source parses through tree-sitter-al with
    /// no errors. Used to guard against generator templates that drift
    /// out-of-sync with the grammar (e.g. property renames, syntax
    /// tightening). A string-content assertion would not catch this.
    fn assert_al_parses(label: &str, source: &str) {
        let result = al_syntax::parser::AlParser::parse_quick(source);
        assert!(
            result.errors.is_empty(),
            "{label} did not parse cleanly:\n{source}\nerrors: {:?}",
            result.errors
        );
    }

    #[test]
    fn generated_starter_codeunit_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_starter_codeunit(&config);
        assert_al_parses("starter codeunit", &src);
    }

    #[test]
    fn generated_library_codeunit_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_library_codeunit(&config);
        assert_al_parses("library codeunit", &src);
    }

    #[test]
    fn generated_test_codeunit_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_test_codeunit(&config);
        assert_al_parses("test codeunit", &src);
    }

    #[test]
    fn generated_copilot_codeunit_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_copilot_codeunit(&config);
        assert_al_parses("copilot participant", &src);
    }

    #[test]
    fn generated_azure_openai_codeunit_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_azure_openai_codeunit(&config);
        assert_al_parses("azure openai helper", &src);
    }

    #[test]
    fn generated_agent_codeunit_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_agent_codeunit(&config);
        assert_al_parses("agent codeunit", &src);
    }

    #[test]
    fn generated_agent_job_handler_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_agent_job_handler(&config);
        assert_al_parses("agent job handler", &src);
    }

    #[test]
    fn generated_api_page_parses() {
        let config = ScaffoldConfig::default();
        let src = generate_api_page(&config);
        assert_al_parses("api page", &src);
    }

    #[test]
    fn generated_codeunit_with_escaped_quote_in_name_parses() {
        // Regression for the iteration-9 escape fix: if a name contains
        // `"`, the doubled-quote escape must produce parseable AL.
        let config = ScaffoldConfig {
            name: r#"My"App"#.to_string(),
            ..Default::default()
        };
        let src = generate_library_codeunit(&config);
        // The header must end up as "My""App Library".
        assert!(src.contains(r#""My""App Library""#));
        assert_al_parses("library codeunit with escaped quote", &src);
    }

    // -- Custom (user-defined) templates — gap C12 ---------------------------

    /// Write a custom template (`template.json` + `files/`) under `root`.
    fn write_custom_template(root: &Path, name: &str, descriptor: &str, files: &[(&str, &str)]) {
        let tdir = root.join(name);
        std::fs::create_dir_all(tdir.join("files")).unwrap();
        std::fs::write(tdir.join("template.json"), descriptor).unwrap();
        for (rel, content) in files {
            let p = tdir.join("files").join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, content).unwrap();
        }
    }

    #[test]
    fn custom_template_materializes_with_substitution() {
        let store = tempfile::tempdir().unwrap();
        write_custom_template(
            store.path(),
            "mytmpl",
            r#"{ "description": "demo", "generateId": true, "idFrom": 60000 }"#,
            &[
                (
                    "app.json",
                    r#"{"id":"{{id}}","name":"{{name}}","publisher":"{{publisher}}","idRanges":[{"from":{{id_from}},"to":{{id_to}}}]}"#,
                ),
                (
                    "src/Main.Codeunit.al",
                    "// {{name}} by {{publisher}} starting at {{id_from}}\n",
                ),
            ],
        );

        let custom = resolve_custom_template_in(store.path(), "mytmpl")
            .unwrap()
            .unwrap();
        let proj = tempfile::tempdir().unwrap();
        let target = proj.path().join("new");
        let config = ScaffoldConfig {
            name: "Acme App".to_string(),
            publisher: "Acme".to_string(),
            id: "00000000-0000-0000-0000-000000000000".to_string(),
            template: ProjectTemplate::Custom(custom),
            ..Default::default()
        };

        let result = create_project(&target, &config).unwrap();
        assert!(result.files_created.iter().any(|f| f == "app.json"));
        assert!(result
            .files_created
            .iter()
            .any(|f| f == "src/Main.Codeunit.al"));
        // No built-in scaffolding is imposed on a custom template.
        assert!(!target.join(".gitignore").exists());
        assert!(!target.join(".zed/debug.json").exists());

        let app = std::fs::read_to_string(target.join("app.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&app).unwrap();
        assert_eq!(parsed["name"], "Acme App");
        assert_eq!(parsed["publisher"], "Acme");
        assert_eq!(parsed["idRanges"][0]["from"], 60000);
        assert_eq!(parsed["idRanges"][0]["to"], 60049);
        // generateId -> a fresh GUID, not the all-zero config id.
        let id = parsed["id"].as_str().unwrap();
        assert_ne!(id, "00000000-0000-0000-0000-000000000000");
        assert_eq!(id.len(), 36);

        let main = std::fs::read_to_string(target.join("src/Main.Codeunit.al")).unwrap();
        assert!(main.contains("Acme App by Acme starting at 60000"));
    }

    #[test]
    fn custom_template_without_generate_id_uses_config_id() {
        let store = tempfile::tempdir().unwrap();
        write_custom_template(
            store.path(),
            "t",
            r#"{}"#,
            &[("app.json", r#"{"id":"{{id}}"}"#)],
        );
        let custom = resolve_custom_template_in(store.path(), "t")
            .unwrap()
            .unwrap();
        let proj = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            id: "11111111-2222-3333-4444-555555555555".to_string(),
            template: ProjectTemplate::Custom(custom),
            ..Default::default()
        };
        create_project(&proj.path().join("p"), &config).unwrap();
        let app = std::fs::read_to_string(proj.path().join("p/app.json")).unwrap();
        assert!(app.contains("11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn custom_template_substitutes_path_placeholders() {
        let store = tempfile::tempdir().unwrap();
        write_custom_template(
            store.path(),
            "t",
            r#"{}"#,
            &[("src/{{name}}.Codeunit.al", "ok")],
        );
        let custom = resolve_custom_template_in(store.path(), "t")
            .unwrap()
            .unwrap();
        let proj = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            name: "Widget".to_string(),
            template: ProjectTemplate::Custom(custom),
            ..Default::default()
        };
        let result = create_project(&proj.path().join("p"), &config).unwrap();
        assert!(
            result
                .files_created
                .iter()
                .any(|f| f == "src/Widget.Codeunit.al"),
            "files: {:?}",
            result.files_created
        );
        assert!(proj.path().join("p/src/Widget.Codeunit.al").exists());
    }

    #[test]
    fn resolve_custom_rejects_traversal_name() {
        let store = tempfile::tempdir().unwrap();
        let err = resolve_custom_template_in(store.path(), "../evil").unwrap_err();
        assert!(err.contains("Invalid template name"), "{err}");
        assert!(resolve_custom_template_in(store.path(), "a/b").is_err());
        assert!(resolve_custom_template_in(store.path(), "..").is_err());
        assert!(resolve_custom_template_in(store.path(), "").is_err());
    }

    #[test]
    fn resolve_custom_unknown_returns_none() {
        let store = tempfile::tempdir().unwrap();
        assert!(resolve_custom_template_in(store.path(), "nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn resolve_custom_missing_descriptor_errors() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(store.path().join("t/files")).unwrap();
        let err = resolve_custom_template_in(store.path(), "t").unwrap_err();
        assert!(err.contains("missing template.json"), "{err}");
    }

    #[test]
    fn resolve_custom_parses_descriptor() {
        let store = tempfile::tempdir().unwrap();
        write_custom_template(
            store.path(),
            "t",
            r#"{"generateId": true, "idFrom": 70000, "idTo": 70099, "description": "d"}"#,
            &[("app.json", "{}")],
        );
        let custom = resolve_custom_template_in(store.path(), "t")
            .unwrap()
            .unwrap();
        assert_eq!(custom.name, "t");
        assert!(custom.descriptor.generate_id);
        assert_eq!(custom.descriptor.id_from, Some(70000));
        assert_eq!(custom.descriptor.id_to, Some(70099));
    }

    #[test]
    fn resolve_custom_malformed_descriptor_errors() {
        let store = tempfile::tempdir().unwrap();
        write_custom_template(store.path(), "t", r#"{ not json"#, &[("app.json", "{}")]);
        let err = resolve_custom_template_in(store.path(), "t").unwrap_err();
        assert!(err.contains("Invalid template.json"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn custom_template_rejects_symlink() {
        let store = tempfile::tempdir().unwrap();
        let tdir = store.path().join("t");
        std::fs::create_dir_all(tdir.join("files")).unwrap();
        std::fs::write(tdir.join("template.json"), "{}").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", tdir.join("files/leak.al")).unwrap();

        let custom = resolve_custom_template_in(store.path(), "t")
            .unwrap()
            .unwrap();
        let proj = tempfile::tempdir().unwrap();
        let config = ScaffoldConfig {
            template: ProjectTemplate::Custom(custom),
            ..Default::default()
        };
        let err = create_project(&proj.path().join("p"), &config).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }

    #[test]
    fn from_str_unknown_template_has_clear_error() {
        let err = "totally-bogus-template-xyz"
            .parse::<ProjectTemplate>()
            .unwrap_err();
        assert!(err.contains("Unknown template"), "{err}");
        assert!(err.contains("totally-bogus-template-xyz"), "{err}");
    }

    #[test]
    fn guard_relative_blocks_parent_and_absolute() {
        assert!(guard_relative(Path::new("a/b.al")).is_ok());
        assert!(guard_relative(Path::new("../a")).is_err());
        assert!(guard_relative(Path::new("a/../../b")).is_err());
        assert!(guard_relative(Path::new("/abs")).is_err());
    }

    #[test]
    fn fresh_guid_is_well_formed_and_distinct() {
        let a = fresh_guid();
        let b = fresh_guid();
        assert_eq!(a.len(), 36);
        assert_ne!(a, b);
        for (i, &c) in a.as_bytes().iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                assert_eq!(c, b'-', "expected hyphen at {i}: {a}");
            } else {
                assert!(c.is_ascii_hexdigit(), "non-hex at {i}: {a}");
            }
        }
        // version-4 nibble
        assert_eq!(a.as_bytes()[14], b'4', "{a}");
    }
}
