//! Tenant guard for every connection to a Business Central cloud tenant.
//!
//! Standard route checked: the launch.json `tenant` field, parsed into
//! `BcServerConfig` and `BcDebugConfig`. It names a tenant but nothing confirms
//! the tenant is the one the person meant. A copied customer launch.json then
//! attaches a debugger to that customer (the 2026-10-26 incident, finding
//! process-02). `BcDebugConfig` also defaulted to the tenant `"default"` and
//! `BcClient` to `"common"`. The guard sits on top of that field. It does not
//! replace it.
//!
//! The contract matches `~/Projects/tools/business-central/tenantguard.py`:
//!
//! 1. A cloud connection needs a tenant GUID. There is no fallback to
//!    `default`, `common` or an earlier run.
//! 2. Commands run from the CLI or MCP pass the tenant GUID typed in full
//!    (`--tenant`). It must equal the launch.json tenant ([`require`]).
//! 3. The tenant must be listed in the registry file (`~/.config/bc-tenants.json`,
//!    or the path in `BC_TENANTS_FILE`). Kind `customer` also needs a purpose.
//! 4. The first connection per tenant and process logs one line with the tenant
//!    name, GUID and kind.
//! 5. Inside the editor there is no `--tenant`. The launch.json tenant is used
//!    and a customer tenant is refused unless the launch configuration carries
//!    `tenantPurpose` ([`require_from_config`]).
//!
//! Scope: Sandbox and Production (cloud) configurations. An on-premises server
//! has no Entra tenant GUID to type and is not in the registry, so callers skip
//! the guard for it.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

/// Prefix of every refusal. The CLI maps a message that carries it to exit code 3.
pub const REFUSAL_PREFIX: &str = "tenant guard refused:";

/// A connection the guard does not allow. The text is the whole reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TenantRefused(pub String);

fn refuse(reason: impl std::fmt::Display) -> TenantRefused {
    TenantRefused(format!("{REFUSAL_PREFIX} {reason}"))
}

/// A tenant the guard allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantInfo {
    pub guid: String,
    pub name: String,
    pub kind: String,
    pub purpose: Option<String>,
}

impl TenantInfo {
    /// The one line logged before the first request.
    #[must_use]
    pub fn banner(&self) -> String {
        let line = format!("tenant {} ({}) {}", self.name, self.guid, self.kind);
        match &self.purpose {
            Some(purpose) => format!("{line}, purpose: {purpose}"),
            None => line,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    name: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    tenants: HashMap<String, RegistryEntry>,
}

/// The tenant registry, keyed by lower case GUID.
#[derive(Debug, Default)]
pub struct Registry {
    path: PathBuf,
    entries: HashMap<String, RegistryEntry>,
}

/// Registry path from an explicit `BC_TENANTS_FILE` value, else the default.
#[must_use]
pub fn registry_path_from(env_value: Option<&str>) -> PathBuf {
    match env_value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(path) => expand_home(path),
        None => expand_home("~/.config/bc-tenants.json"),
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

/// The registry file this process reads: `BC_TENANTS_FILE` or the default.
#[must_use]
pub fn registry_path() -> PathBuf {
    registry_path_from(std::env::var("BC_TENANTS_FILE").ok().as_deref())
}

/// Read a registry file.
pub fn load_registry(path: &Path) -> Result<Registry, TenantRefused> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        refuse(format!(
            "tenant registry {} cannot be read ({error}). Create it before connecting to Business Central.",
            path.display()
        ))
    })?;
    let file: RegistryFile = serde_json::from_str(&text).map_err(|error| {
        refuse(format!(
            "tenant registry {} is not valid ({error}).",
            path.display()
        ))
    })?;
    Ok(Registry {
        path: path.to_path_buf(),
        entries: file
            .tenants
            .into_iter()
            .map(|(guid, entry)| (guid.to_ascii_lowercase(), entry))
            .collect(),
    })
}

/// True for the 8-4-4-4-12 hex form of a tenant GUID.
#[must_use]
pub fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn lookup(
    guid: &str,
    purpose: Option<&str>,
    registry: &Registry,
) -> Result<TenantInfo, TenantRefused> {
    let Some(entry) = registry.entries.get(guid) else {
        return Err(refuse(format!(
            "tenant {guid} is not in {}. Add it with its name and kind (demo or customer) first.",
            registry.path.display()
        )));
    };
    let kind = entry.kind.clone().unwrap_or_else(|| "customer".to_string());
    let name = entry.name.clone().unwrap_or_else(|| guid.to_string());
    let purpose = purpose
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    if kind != "demo" && purpose.is_none() {
        return Err(refuse(format!(
            "{name} ({guid}) is a {kind} tenant. Tests of tools, extensions or prototypes go to a \
             demo tenant. For work on this customer's own issue, add a purpose naming the issue \
             (--purpose on the command line, `tenantPurpose` in the launch configuration)."
        )));
    }
    Ok(TenantInfo {
        guid: guid.to_string(),
        name,
        kind,
        purpose,
    })
}

fn config_guid(config_tenant: Option<&str>) -> Result<String, TenantRefused> {
    let Some(config) = config_tenant.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(refuse(
            "the launch configuration has no `tenant`. Set `tenant` to the tenant GUID of the \
             environment. It is never defaulted.",
        ));
    };
    if !is_guid(config) || config.eq_ignore_ascii_case("default") {
        return Err(refuse(format!(
            "the `tenant` in the launch configuration ({config}) must be the tenant GUID, not a \
             domain name or \"default\"."
        )));
    }
    Ok(config.to_ascii_lowercase())
}

/// Strict check for CLI and MCP callers: the tenant is typed on the command
/// and must equal the launch.json tenant.
pub fn require(
    cli_tenant: Option<&str>,
    purpose: Option<&str>,
    config_tenant: Option<&str>,
    registry: &Registry,
) -> Result<TenantInfo, TenantRefused> {
    let Some(typed) = cli_tenant.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(refuse(
            "pass --tenant <GUID>. The tenant is never taken from launch.json, a saved login or \
             an earlier run.",
        ));
    };
    if !is_guid(typed) {
        return Err(refuse(format!(
            "--tenant must be a full tenant GUID, got '{typed}'."
        )));
    }
    let guid = typed.to_ascii_lowercase();
    let from_config = config_guid(config_tenant)?;
    if from_config != guid {
        return Err(refuse(format!(
            "--tenant {guid} does not match the tenant in the launch configuration \
             ({from_config}). Fix the configuration or the command, do not guess."
        )));
    }
    lookup(&guid, purpose, registry)
}

/// Editor check: there is no typed tenant, so the launch.json tenant is the
/// tenant, and a customer tenant needs the configuration's purpose.
pub fn require_from_config(
    config_tenant: Option<&str>,
    config_purpose: Option<&str>,
    registry: &Registry,
) -> Result<TenantInfo, TenantRefused> {
    let guid = config_guid(config_tenant)?;
    lookup(&guid, config_purpose, registry)
}

/// What the caller of a request typed: `--tenant` and `--purpose`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declaration {
    pub tenant: Option<String>,
    pub purpose: Option<String>,
}

tokio::task_local! {
    static DECLARED: Declaration;
}

/// Run `future` as a request from a CLI or MCP caller. Connections made inside
/// it use the strict check against `declaration`. A connection made outside any
/// scope (the editor) uses the launch configuration alone.
pub async fn scope_declaration<F: Future>(declaration: Declaration, future: F) -> F::Output {
    DECLARED.scope(declaration, future).await
}

static REGISTRY_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Point the guard at a registry file for the rest of the process. For tests
/// only: they run in parallel threads, so they cannot set `BC_TENANTS_FILE`.
#[cfg(any(test, feature = "test-support"))]
pub fn set_registry_override(path: &Path) {
    *REGISTRY_OVERRIDE.lock().unwrap() = Some(path.to_path_buf());
}

/// The CDX demo tenant, the only cloud tenant tests use.
#[cfg(any(test, feature = "test-support"))]
pub const TEST_DEMO_TENANT: &str = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d";

/// Write a throwaway registry with the CDX demo tenant, point the guard at it
/// and return the CDX GUID. Safe to call from every test, it runs once.
#[cfg(any(test, feature = "test-support"))]
pub fn install_test_registry() -> &'static str {
    static INSTALLED: OnceLock<PathBuf> = OnceLock::new();
    let path = INSTALLED.get_or_init(|| {
        let path =
            std::env::temp_dir().join(format!("al-bc-test-tenants-{}.json", std::process::id()));
        let body =
            format!(r#"{{"tenants":{{"{TEST_DEMO_TENANT}":{{"name":"CDX","kind":"demo"}}}}}}"#);
        std::fs::write(&path, body).expect("write the test registry");
        path
    });
    set_registry_override(path);
    TEST_DEMO_TENANT
}

fn current_registry() -> Result<Registry, TenantRefused> {
    let override_path = REGISTRY_OVERRIDE
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    load_registry(&override_path.unwrap_or_else(registry_path))
}

fn announced() -> &'static Mutex<HashSet<String>> {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Log the banner the first time this process connects to a tenant.
fn announce_once(info: &TenantInfo) {
    let first = announced()
        .lock()
        .map(|mut seen| seen.insert(info.guid.clone()))
        .unwrap_or(true);
    if first {
        tracing::info!("{}", info.banner());
    }
}

/// The guard every cloud connection calls before its first request.
///
/// Inside [`scope_declaration`] (a CLI or MCP request) the typed tenant must be
/// present and equal `config_tenant`. Outside it (the editor) `config_tenant`
/// alone is used and `config_purpose` stands in for `--purpose`.
pub fn guard_cloud_connection(
    config_tenant: Option<&str>,
    config_purpose: Option<&str>,
) -> Result<TenantInfo, TenantRefused> {
    let registry = current_registry()?;
    let info = match DECLARED.try_with(Clone::clone) {
        Ok(declared) => require(
            declared.tenant.as_deref(),
            declared.purpose.as_deref().or(config_purpose),
            config_tenant,
            &registry,
        )?,
        Err(_) => require_from_config(config_tenant, config_purpose, &registry)?,
    };
    announce_once(&info);
    Ok(info)
}

/// The banner line for a typed tenant, when the registry lists it. The CLI
/// prints it before the first request. A tenant the registry does not list
/// gets no line here and a refusal from the daemon.
#[must_use]
pub fn banner_for(tenant: &str, purpose: Option<&str>) -> Option<String> {
    let registry = current_registry().ok()?;
    let guid = tenant.trim().to_ascii_lowercase();
    let entry = registry.entries.get(&guid)?;
    Some(
        TenantInfo {
            guid: guid.clone(),
            name: entry.name.clone().unwrap_or_else(|| guid.clone()),
            kind: entry.kind.clone().unwrap_or_else(|| "customer".to_string()),
            purpose: purpose
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        }
        .banner(),
    )
}

/// The refusal for a cloud configuration or request that names no tenant at
/// all, for places that build a URL and have nothing to guard yet.
#[must_use]
pub fn missing_tenant_message(what: &str) -> String {
    format!(
        "{what} has no `tenant`. A cloud connection needs the tenant GUID in the launch \
         configuration. There is no fallback to \"default\" or \"common\"."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DMS: &str = "5c823c3b-d5c9-4853-b74c-d59b81265f78";
    const CDX: &str = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d";

    /// The incident shape: DMS is a customer tenant. These tests make no
    /// network call, they stop at the guard.
    fn registry() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bc-tenants.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"tenants":{{
                "{CDX}":{{"name":"CDX","kind":"demo"}},
                "{DMS}":{{"name":"DMS","kind":"customer"}}}}}}"#
            ),
        )
        .unwrap();
        let registry = load_registry(&path).unwrap();
        (dir, registry)
    }

    #[test]
    fn config_names_dms_and_no_tenant_is_refused() {
        let (_dir, registry) = registry();
        let error = require(None, None, Some(DMS), &registry).unwrap_err();
        assert!(error.0.contains("--tenant"), "{error}");
        assert!(error.0.starts_with(REFUSAL_PREFIX), "{error}");
    }

    #[test]
    fn cdx_tenant_with_dms_config_is_refused() {
        let (_dir, registry) = registry();
        let error = require(Some(CDX), None, Some(DMS), &registry).unwrap_err();
        assert!(error.0.contains("does not match"), "{error}");
    }

    #[test]
    fn dms_without_purpose_is_refused() {
        let (_dir, registry) = registry();
        let error = require(Some(DMS), None, Some(DMS), &registry).unwrap_err();
        assert!(error.0.contains("customer tenant"), "{error}");
        assert!(require(Some(DMS), Some("  "), Some(DMS), &registry).is_err());
    }

    #[test]
    fn dms_with_purpose_passes_and_banner_names_it() {
        let (_dir, registry) = registry();
        let info = require(Some(DMS), Some("ticket 123"), Some(DMS), &registry).unwrap();
        assert_eq!(info.name, "DMS");
        assert_eq!(
            info.banner(),
            format!("tenant DMS ({DMS}) customer, purpose: ticket 123")
        );
    }

    #[test]
    fn cdx_with_cdx_config_passes_without_purpose() {
        let (_dir, registry) = registry();
        let info = require(Some(CDX), None, Some(CDX), &registry).unwrap();
        assert_eq!(info.kind, "demo");
        assert_eq!(info.banner(), format!("tenant CDX ({CDX}) demo"));
    }

    #[test]
    fn config_without_tenant_names_tenant() {
        let (_dir, registry) = registry();
        for config in [
            None,
            Some(""),
            Some("default"),
            Some("common"),
            Some("contoso.onmicrosoft.com"),
        ] {
            let error = require(Some(CDX), None, config, &registry).unwrap_err();
            assert!(
                error.0.contains("`tenant`") || error.0.contains("tenant GUID"),
                "{error}"
            );
            let error = require_from_config(config, None, &registry).unwrap_err();
            assert!(error.0.contains("tenant"), "{error}");
        }
    }

    #[test]
    fn unregistered_and_malformed_tenants_are_refused() {
        let (_dir, registry) = registry();
        let other = "00000000-0000-0000-0000-000000000000";
        let error = require(Some(other), None, Some(other), &registry).unwrap_err();
        assert!(error.0.contains("is not in"), "{error}");
        let error = require(Some("cdx"), None, Some(CDX), &registry).unwrap_err();
        assert!(error.0.contains("full tenant GUID"), "{error}");
    }

    #[test]
    fn editor_mode_needs_the_configuration_purpose_for_a_customer() {
        let (_dir, registry) = registry();
        assert!(require_from_config(Some(DMS), None, &registry).is_err());
        let info = require_from_config(Some(DMS), Some("ticket 123"), &registry).unwrap();
        assert_eq!(info.kind, "customer");
        assert!(require_from_config(Some(CDX), None, &registry).is_ok());
    }

    #[test]
    fn guid_case_is_ignored() {
        let (_dir, registry) = registry();
        let upper = CDX.to_ascii_uppercase();
        assert!(require(Some(&upper), None, Some(CDX), &registry).is_ok());
    }

    #[test]
    fn registry_path_honours_the_env_value() {
        assert_eq!(
            registry_path_from(Some("/x/t.json")),
            PathBuf::from("/x/t.json")
        );
        assert!(registry_path_from(None).ends_with(".config/bc-tenants.json"));
        assert!(registry_path_from(Some(" ")).ends_with(".config/bc-tenants.json"));
    }

    #[test]
    fn missing_registry_is_a_refusal() {
        let error = load_registry(Path::new("/nonexistent/bc-tenants.json")).unwrap_err();
        assert!(error.0.starts_with(REFUSAL_PREFIX), "{error}");
    }

    #[tokio::test]
    async fn scope_makes_the_guard_strict() {
        let (dir, _registry) = registry();
        set_registry_override(&dir.path().join("bc-tenants.json"));
        // Editor mode: the configuration tenant alone passes for a demo tenant.
        assert!(guard_cloud_connection(Some(CDX), None).is_ok());
        // CLI mode with no --tenant is refused, with a mismatch too.
        let none = scope_declaration(Declaration::default(), async {
            guard_cloud_connection(Some(CDX), None)
        })
        .await;
        assert!(none.unwrap_err().0.contains("--tenant"));
        let typed = Declaration {
            tenant: Some(CDX.to_string()),
            purpose: None,
        };
        let ok = scope_declaration(typed, async { guard_cloud_connection(Some(CDX), None) }).await;
        assert!(ok.is_ok());
        std::mem::forget(dir);
    }
}
