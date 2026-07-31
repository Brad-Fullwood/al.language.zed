//! Regenerate the checked-in Record method catalog from Microsoft's AL toolchain.
//!
//! Usage:
//! `cargo run -p al-semantic --features semantic --example export_record_methods -- \
//!   <Microsoft.Dynamics.Nav.CodeAnalysis.dll> <record_methods.json>`

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use al_semantic::SemanticBridge;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordMethodCatalog {
    source_type: &'static str,
    toolchain_version: String,
    methods: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("record-method export failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let (code_analysis, output) = parse_args(std::env::args_os().skip(1))?;
    if !code_analysis.is_file() {
        return Err(format!(
            "CodeAnalysis assembly does not exist: {}",
            code_analysis.display()
        )
        .into());
    }

    let metadata = code_analysis.metadata()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let toolchain_version =
        discover_extension_version(&code_analysis).unwrap_or_else(|| "unknown".to_string());
    let cache_version = format!(
        "record-method-catalog-{toolchain_version}-{}-{modified}",
        metadata.len()
    );

    let bridge = SemanticBridge::new(&code_analysis, &cache_version)?;
    let builtins = bridge.builtin_types_fresh().await?;
    let table_class = builtins
        .iter()
        .find(|builtin| builtin.name.eq_ignore_ascii_case("TableClass"))
        .ok_or("Microsoft built-in catalog has no TableClass type")?;
    let methods: Vec<String> = table_class
        .methods
        .iter()
        .map(|method| method.name.trim())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if methods.len() < 50 {
        return Err(format!(
            "TableClass catalog is implausibly small: {} unique methods",
            methods.len()
        )
        .into());
    }

    let catalog = RecordMethodCatalog {
        source_type: "TableClass",
        toolchain_version,
        methods,
    };
    let mut bytes = serde_json::to_vec_pretty(&catalog)?;
    bytes.push(b'\n');
    persist_atomically(&output, &bytes)?;

    eprintln!(
        "wrote {} Record methods to {}",
        catalog.methods.len(),
        output.display()
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let args: Vec<OsString> = args.collect();
    if args.len() != 2 {
        return Err(
            "usage: export_record_methods <Microsoft.Dynamics.Nav.CodeAnalysis.dll> <output.json>"
                .into(),
        );
    }
    Ok((PathBuf::from(&args[0]), PathBuf::from(&args[1])))
}

/// Resolve the toolchain version that produced `code_analysis`.
///
/// Microsoft ships the AL compiler in two supported layouts, and both must
/// report a real version — an "unknown" provenance stamp turns every
/// `check-record-methods` run into a spurious drift failure:
///
/// 1. the VS Code extension (`ms-dynamics-smb.al-<version>/bin/<platform>`),
///    whose version lives in the extension's `package.json`; and
/// 2. the `microsoft.dynamics.businesscentral.development.tools` dotnet tool,
///    which has no `package.json` but keeps its NuGet `.nuspec` above
///    `tools/<tfm>/any`.
fn discover_extension_version(code_analysis: &Path) -> Option<String> {
    for ancestor in code_analysis.ancestors() {
        if let Some(version) = vscode_extension_version(ancestor) {
            return Some(version);
        }
        if let Some(version) = nuget_package_version(ancestor) {
            return Some(version);
        }
    }
    None
}

/// Version from a VS Code extension manifest directly inside `dir`.
fn vscode_extension_version(dir: &Path) -> Option<String> {
    let bytes = fs::read(dir.join("package.json")).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    let is_al_extension = value.get("name").and_then(serde_json::Value::as_str) == Some("al")
        || value
            .get("publisher")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|publisher| publisher.eq_ignore_ascii_case("microsoft"));
    if !is_al_extension {
        return None;
    }
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

/// Version from a NuGet `.nuspec` directly inside `dir`, as installed by
/// `dotnet tool install microsoft.dynamics.businesscentral.development.tools`.
fn nuget_package_version(dir: &Path) -> Option<String> {
    let entries = fs::read_dir(dir).ok()?;
    let mut nuspecs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nuspec"))
        })
        .collect();
    // Deterministic pick when a layout ever ships more than one manifest.
    nuspecs.sort();
    for nuspec in nuspecs {
        let text = match fs::read_to_string(&nuspec) {
            Ok(text) => text,
            Err(_) => continue,
        };
        if let Some(version) = xml_element_text(&text, "version") {
            return Some(version);
        }
    }
    None
}

/// First `<tag>…</tag>` body in `xml`, trimmed. Deliberately a plain scan: the
/// only input is Microsoft's own generated `.nuspec`, so an XML parser would be
/// a dependency without a contract to enforce.
fn xml_element_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    let text = xml[start..end].trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn persist_atomically(output: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<ext>/bin/linux/Microsoft.Dynamics.Nav.CodeAnalysis.dll` alongside the
    /// extension manifest two levels up.
    fn vscode_layout(root: &Path, version: &str) -> PathBuf {
        let ext = root.join(format!("ms-dynamics-smb.al-{version}"));
        let bin = ext.join("bin").join("linux");
        fs::create_dir_all(&bin).unwrap();
        fs::write(
            ext.join("package.json"),
            format!(r#"{{"name":"al","publisher":"ms-dynamics-smb","version":"{version}"}}"#),
        )
        .unwrap();
        let dll = bin.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
        fs::write(&dll, b"").unwrap();
        dll
    }

    /// `dotnet tool install microsoft.dynamics.businesscentral.development.tools`
    /// layout: no `package.json` anywhere, `.nuspec` above `tools/<tfm>/any`.
    fn dotnet_tool_layout(root: &Path, version: &str) -> PathBuf {
        let pkg = root
            .join(".store")
            .join("microsoft.dynamics.businesscentral.development.tools")
            .join(version)
            .join("microsoft.dynamics.businesscentral.development.tools")
            .join(version);
        let any = pkg.join("tools").join("net8.0").join("any");
        fs::create_dir_all(&any).unwrap();
        fs::write(
            pkg.join("Microsoft.Dynamics.BusinessCentral.Development.Tools.nuspec"),
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package><metadata>\
                 <id>Microsoft.Dynamics.BusinessCentral.Development.Tools</id>\
                 <version>{version}</version></metadata></package>\n"
            ),
        )
        .unwrap();
        let dll = any.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
        fs::write(&dll, b"").unwrap();
        dll
    }

    #[test]
    fn vscode_extension_layout_reports_the_extension_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dll = vscode_layout(tmp.path(), "17.0.2273547");
        assert_eq!(
            discover_extension_version(&dll).as_deref(),
            Some("17.0.2273547")
        );
    }

    /// Regression: this layout previously fell through to "unknown", so
    /// `make check-record-methods` reported drift against a toolchain whose
    /// method catalog matched exactly.
    #[test]
    fn dotnet_tool_layout_reports_the_nuget_package_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dll = dotnet_tool_layout(tmp.path(), "17.0.34.45391");
        assert_eq!(
            discover_extension_version(&dll).as_deref(),
            Some("17.0.34.45391")
        );
    }

    #[test]
    fn a_layout_with_neither_manifest_reports_no_version() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin").join("linux");
        fs::create_dir_all(&bin).unwrap();
        let dll = bin.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
        fs::write(&dll, b"").unwrap();
        assert_eq!(discover_extension_version(&dll), None);
    }

    #[test]
    fn an_unrelated_package_json_does_not_supply_a_version() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"some-other-tool","publisher":"contoso","version":"9.9.9"}"#,
        )
        .unwrap();
        let dll = bin.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
        fs::write(&dll, b"").unwrap();
        assert_eq!(discover_extension_version(&dll), None);
    }

    #[test]
    fn the_nearest_manifest_wins_over_one_further_up() {
        let tmp = tempfile::tempdir().unwrap();
        // An outer nuspec must not shadow the extension manifest next to the DLL.
        fs::write(
            tmp.path().join("outer.nuspec"),
            "<package><metadata><version>1.0.0</version></metadata></package>",
        )
        .unwrap();
        let dll = vscode_layout(tmp.path(), "17.0.2273547");
        assert_eq!(
            discover_extension_version(&dll).as_deref(),
            Some("17.0.2273547")
        );
    }

    #[test]
    fn nuspec_without_a_version_element_is_not_treated_as_a_version() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("broken.nuspec"),
            "<package><metadata><id>NoVersion</id></metadata></package>",
        )
        .unwrap();
        let dll = tmp.path().join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
        fs::write(&dll, b"").unwrap();
        assert_eq!(discover_extension_version(&dll), None);
    }

    #[test]
    fn xml_element_text_reads_the_first_element_and_trims_it() {
        assert_eq!(
            xml_element_text("<a><version> 1.2.3 </version></a>", "version").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(xml_element_text("<version></version>", "version"), None);
        assert_eq!(xml_element_text("<version>1.0", "version"), None);
        assert_eq!(xml_element_text("<other>1.0</other>", "version"), None);
    }
}
