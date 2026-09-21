//! `NavxManifest.xml` generator — derives the package manifest from `app.json`.
//!
//! Structure + attribute casing match alc 17.x output (verified by inspecting a
//! compiled `.app`). Note alc's own casing quirk, reproduced here: `ShowMyCode`
//! uses `True`/`False` (PascalCase) while `ResourceExposurePolicy` flags use
//! `true`/`false` (lowercase).

const NAVX_NS: &str = "http://schemas.microsoft.com/navx/2015/manifest";

#[derive(Debug, Clone)]
pub struct Dependency {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// The IP-protection policy recorded in the manifest. Server-enforced — the
/// source still ships in the `.app`; the server refuses to serve it.
///
/// The four flags are the ones `app.json` documents: `allowDebugging`,
/// `allowDownloadingSource`, `includeSourceInSymbolFile` and
/// `applyToDevExtension` (devenv-security-settings-and-ip-protection). Each
/// defaults to `false`. alc 17.0.34.45391 writes all four attributes, in the
/// order below, whenever `app.json` declares the object at all, and a
/// self-closing `<ResourceExposurePolicy />` when it does not — which is why
/// `declared` is tracked separately from the flags.
#[derive(Debug, Clone, Default)]
pub struct ResourceExposurePolicy {
    pub declared: bool,
    pub allow_debugging: bool,
    pub allow_downloading_source: bool,
    pub include_source_in_symbol_file: bool,
    pub apply_to_dev_extension: bool,
}

/// Inputs for `NavxManifest.xml`, mostly mirroring `app.json`.
#[derive(Debug, Clone)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub brief: String,
    pub description: String,
    /// Optional `<App>` metadata alc always emits (empty when unset in app.json).
    pub privacy_statement: String,
    pub eula: String,
    pub help: String,
    pub help_base_url: String,
    pub url: String,
    pub logo: String,
    pub platform: String,
    pub application: String,
    pub runtime: String,
    pub target: String,
    pub show_my_code: bool,
    pub id_ranges: Vec<(i64, i64)>,
    pub dependencies: Vec<Dependency>,
    pub resource_exposure_policy: ResourceExposurePolicy,
    /// `<Build CompilerVersion="…">` — the producing compiler's version.
    pub compiler_version: String,
    /// `<Build Timestamp="…">` — ISO-8601 build time (caller-supplied so this
    /// stays deterministic / testable).
    pub build_timestamp: String,
}

impl AppManifest {
    /// Build from a parsed `app.json` value, filling alc's defaults for fields
    /// `app.json` does not carry.
    pub fn from_app_json(
        app: &serde_json::Value,
        compiler_version: &str,
        build_timestamp: &str,
    ) -> Self {
        let s = |k: &str| {
            app.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let id_ranges = app
            .get("idRanges")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        let from = r.get("from").and_then(|v| v.as_i64())?;
                        let to = r.get("to").and_then(|v| v.as_i64())?;
                        Some((from, to))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let dependencies = app
            .get("dependencies")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|d| Dependency {
                        id: d
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: d
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        publisher: d
                            .get("publisher")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        version: d
                            .get("version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rep = app
            .get("resourceExposurePolicy")
            .filter(|value| value.is_object());
        let rep_bool = |k: &str| {
            rep.and_then(|r| r.get(k))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        AppManifest {
            id: s("id"),
            name: s("name"),
            publisher: s("publisher"),
            version: s("version"),
            brief: s("brief"),
            description: s("description"),
            privacy_statement: s("privacyStatement"),
            eula: s("EULA"),
            help: s("help"),
            help_base_url: s("helpBaseUrl"),
            url: s("url"),
            logo: s("logo"),
            platform: s("platform"),
            application: s("application"),
            runtime: s("runtime"),
            target: if app.get("target").and_then(|v| v.as_str()).is_some() {
                s("target")
            } else {
                "Cloud".to_string()
            },
            show_my_code: app
                .get("showMyCode")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            id_ranges,
            dependencies,
            resource_exposure_policy: ResourceExposurePolicy {
                declared: rep.is_some(),
                allow_debugging: rep_bool("allowDebugging"),
                allow_downloading_source: rep_bool("allowDownloadingSource"),
                include_source_in_symbol_file: rep_bool("includeSourceInSymbolFile"),
                apply_to_dev_extension: rep_bool("applyToDevExtension"),
            },
            compiler_version: compiler_version.to_string(),
            build_timestamp: build_timestamp.to_string(),
        }
    }

    /// Render the `NavxManifest.xml` document. alc emits this part 2-space
    /// indented with no XML prolog and no trailing newline.
    pub fn to_navx_xml(&self) -> String {
        let e = xml_escape_attr;
        let mut x = String::new();
        x.push_str(&format!("<Package xmlns=\"{NAVX_NS}\">\n"));
        // alc OMITS the Platform / Application attributes entirely when they are
        // unset in app.json (unlike Brief/Description/etc., which alc emits as
        // empty strings). Match that exactly so the manifest is byte-identical to
        // alc's: emit each attribute only when non-empty.
        let opt_attr = |name: &str, val: &str| {
            if val.is_empty() {
                String::new()
            } else {
                format!(" {name}=\"{}\"", e(val))
            }
        };
        x.push_str(&format!(
            "  <App Id=\"{}\" Name=\"{}\" Publisher=\"{}\" Brief=\"{}\" Description=\"{}\" \
             Version=\"{}\" CompatibilityId=\"0.0.0.0\" PrivacyStatement=\"{}\" EULA=\"{}\" \
             Help=\"{}\" HelpBaseUrl=\"{}\" Url=\"{}\" Logo=\"{}\"{}{} \
             Runtime=\"{}\" Target=\"{}\" ShowMyCode=\"{}\" />\n",
            e(&self.id),
            e(&self.name),
            e(&self.publisher),
            e(&self.brief),
            e(&self.description),
            e(&self.version),
            e(&self.privacy_statement),
            e(&self.eula),
            e(&self.help),
            e(&self.help_base_url),
            e(&self.url),
            e(&self.logo),
            opt_attr("Platform", &self.platform),
            opt_attr("Application", &self.application),
            e(&self.runtime),
            e(&self.target),
            if self.show_my_code { "True" } else { "False" },
        ));

        if self.id_ranges.is_empty() {
            x.push_str("  <IdRanges />\n");
        } else {
            x.push_str("  <IdRanges>\n");
            for (from, to) in &self.id_ranges {
                x.push_str(&format!(
                    "    <IdRange MinObjectId=\"{from}\" MaxObjectId=\"{to}\" />\n"
                ));
            }
            x.push_str("  </IdRanges>\n");
        }

        if self.dependencies.is_empty() {
            x.push_str("  <Dependencies />\n");
        } else {
            x.push_str("  <Dependencies>\n");
            for d in &self.dependencies {
                x.push_str(&format!(
                    "    <Dependency Id=\"{}\" Name=\"{}\" Publisher=\"{}\" MinVersion=\"{}\" CompatibilityId=\"0.0.0.0\" />\n",
                    e(&d.id),
                    e(&d.name),
                    e(&d.publisher),
                    e(&d.version),
                ));
            }
            x.push_str("  </Dependencies>\n");
        }

        x.push_str("  <InternalsVisibleTo />\n  <ScreenShots />\n  <SupportedLocales />\n");
        x.push_str("  <Features />\n  <PreprocessorSymbols />\n  <SuppressWarnings />\n");

        let rep = &self.resource_exposure_policy;
        if rep.declared {
            x.push_str(&format!(
                "  <ResourceExposurePolicy AllowDebugging=\"{}\" AllowDownloadingSource=\"{}\" \
                 IncludeSourceInSymbolFile=\"{}\" ApplyToDevExtension=\"{}\" />\n",
                rep.allow_debugging,
                rep.allow_downloading_source,
                rep.include_source_in_symbol_file,
                rep.apply_to_dev_extension,
            ));
        } else {
            x.push_str("  <ResourceExposurePolicy />\n");
        }

        x.push_str("  <KeyVaultUrls />\n  <Source />\n");
        x.push_str(&format!(
            "  <Build Timestamp=\"{}\" CompilerVersion=\"{}\" />\n",
            e(&self.build_timestamp),
            e(&self.compiler_version),
        ));
        x.push_str("  <AlternateIds />\n");
        x.push_str("</Package>");
        x
    }
}

pub(super) fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a string for use in XML element text content (no quote escaping).
pub(super) fn xml_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::manifest::parse_manifest;

    fn sample() -> AppManifest {
        AppManifest::from_app_json(
            &serde_json::json!({
                "id": "aaaaaaaa-1111-2222-3333-444444444444",
                "name": "Min App",
                "publisher": "Spike",
                "version": "1.0.0.0",
                "platform": "26.0.0.0",
                "application": "26.5.0.0",
                "runtime": "14.0",
                "target": "Cloud",
                "idRanges": [{ "from": 50100, "to": 50149 }],
                "dependencies": []
            }),
            "17.0.34.45391",
            "2026-06-15T20:58:33.0000000Z",
        )
    }

    #[test]
    fn round_trips_through_our_manifest_parser() {
        // The XML we generate must parse with the same reader that reads alc's
        // manifests, recovering the identifying metadata.
        let xml = sample().to_navx_xml();
        let m = parse_manifest(xml.as_bytes()).expect("generated manifest must parse");
        assert_eq!(m.app_id, "aaaaaaaa-1111-2222-3333-444444444444");
        assert_eq!(m.name, "Min App");
        assert_eq!(m.publisher, "Spike");
        assert_eq!(m.version, "1.0.0.0");
    }

    #[test]
    fn id_ranges_and_target_default() {
        let xml = sample().to_navx_xml();
        assert!(xml.contains("<IdRange MinObjectId=\"50100\" MaxObjectId=\"50149\" />"));
        assert!(xml.contains("Target=\"Cloud\""));
        assert!(xml.contains("ShowMyCode=\"False\""));
        assert!(xml.contains("<ResourceExposurePolicy />"));
        assert!(xml.contains("<Dependencies />"));
    }

    /// Build a manifest from `sample()`'s app.json plus a `resourceExposurePolicy`.
    fn with_policy(policy: serde_json::Value) -> AppManifest {
        AppManifest::from_app_json(
            &serde_json::json!({
                "id": "aaaaaaaa-1111-2222-3333-444444444444",
                "name": "Min App", "publisher": "Spike", "version": "1.0.0.0",
                "runtime": "15.0", "target": "Cloud",
                "idRanges": [{ "from": 50100, "to": 50149 }],
                "resourceExposurePolicy": policy,
            }),
            "17.0.34.45391",
            "2026-06-15T20:58:33.0000000Z",
        )
    }

    #[test]
    fn resource_exposure_policy_reads_every_documented_key() {
        // Pinned against alc 17.0.34.45391 output for the same app.json: all
        // four attributes, in this order, whenever the object is declared.
        // `includeSourceInSymbolFile` used to be read under the name
        // `includeSourceInPackageFile`, which no app.json writes, so the flag
        // the developer set was dropped from the package.
        let xml = with_policy(serde_json::json!({
            "applyToDevExtension": true,
            "allowDebugging": true,
            "allowDownloadingSource": false,
            "includeSourceInSymbolFile": true,
        }))
        .to_navx_xml();
        assert!(
            xml.contains(
                "<ResourceExposurePolicy AllowDebugging=\"true\" AllowDownloadingSource=\"false\" \
                 IncludeSourceInSymbolFile=\"true\" ApplyToDevExtension=\"true\" />"
            ),
            "{xml}"
        );
    }

    #[test]
    fn resource_exposure_policy_defaults_unset_keys_to_false() {
        let xml = with_policy(serde_json::json!({ "allowDebugging": true })).to_navx_xml();
        assert!(
            xml.contains(
                "<ResourceExposurePolicy AllowDebugging=\"true\" AllowDownloadingSource=\"false\" \
                 IncludeSourceInSymbolFile=\"false\" ApplyToDevExtension=\"false\" />"
            ),
            "{xml}"
        );
        let empty = with_policy(serde_json::json!({})).to_navx_xml();
        assert!(
            empty.contains("<ResourceExposurePolicy AllowDebugging=\"false\""),
            "a declared but empty policy still writes all four attributes:\n{empty}"
        );
    }

    #[test]
    fn every_schema_resource_exposure_policy_key_reaches_the_manifest() {
        // schemas/app.json is what the editor validates against, so a key it
        // offers that the emitter ignores is a setting silently dropped.
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/app.json"
        )))
        .expect("schemas/app.json parses");
        let keys = schema["properties"]["resourceExposurePolicy"]["properties"]
            .as_object()
            .expect("resourceExposurePolicy declares properties");
        assert!(!keys.is_empty());
        for key in keys.keys() {
            let xml = with_policy(serde_json::json!({ key.as_str(): true })).to_navx_xml();
            let attribute = format!("{}{}=\"true\"", key[..1].to_uppercase(), &key[1..]);
            assert!(
                xml.contains(&attribute),
                "schemas/app.json key {key} never reaches the manifest as {attribute}:\n{xml}"
            );
        }
    }

    #[test]
    fn escapes_special_characters_in_attributes() {
        let mut m = sample();
        m.publisher = "A & B <Co>".to_string();
        let xml = m.to_navx_xml();
        assert!(xml.contains("Publisher=\"A &amp; B &lt;Co&gt;\""));
        // Still parses.
        let parsed = parse_manifest(xml.as_bytes()).unwrap();
        assert_eq!(parsed.publisher, "A & B <Co>");
    }
}
