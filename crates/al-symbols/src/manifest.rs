//! NavxManifest.xml parser.
//!
//! The `NavxManifest.xml` file inside `.app` packages contains metadata:
//! AppId, Name, Publisher, Version, and dependency information.

use quick_xml::events::Event;
use quick_xml::Reader;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("XML parsing error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("Missing required element: {0}")]
    MissingElement(String),
    #[error("Invalid UTF-8 in manifest")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("Invalid attribute: {0}")]
    InvalidAttribute(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDependency {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub min_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavxManifest {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub dependencies: Vec<ManifestDependency>,
}

/// The manifest XML typically looks like:
/// ```xml
/// <?xml version="1.0" encoding="utf-8"?>
/// <Package xmlns="...">
///   <App Id="..." Name="..." Publisher="..." Version="..." ... />
///   ...
/// </Package>
/// ```
pub fn parse_manifest(xml_bytes: &[u8]) -> Result<NavxManifest, ManifestError> {
    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);

    let mut app_id = None;
    let mut name = None;
    let mut publisher = None;
    let mut version = None;
    let mut dependencies = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                let local_name = e.local_name();
                if local_name.as_ref() == b"App" {
                    for attr in e.attributes() {
                        let attr =
                            attr.map_err(|e| ManifestError::InvalidAttribute(format!("{e}")))?;
                        let local = attr.key.local_name();
                        let key = std::str::from_utf8(local.as_ref())?;
                        // quick-xml 0.41 renamed `unescape_value` →
                        // `normalized_value` (same unescaping semantics plus
                        // XML attribute-value normalization). NavxManifest.xml
                        // carries no version declaration → XML 1.0 per spec.
                        let val = attr
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map_err(ManifestError::Xml)?;
                        match key {
                            "Id" => app_id = Some(val.to_string()),
                            "Name" => name = Some(val.to_string()),
                            "Publisher" => publisher = Some(val.to_string()),
                            "Version" => version = Some(val.to_string()),
                            _ => {}
                        }
                    }
                } else if local_name.as_ref() == b"Dependency" {
                    let mut dependency_id = None;
                    let mut dependency_name = None;
                    let mut dependency_publisher = None;
                    let mut dependency_version = None;
                    for attr in e.attributes() {
                        let attr = attr
                            .map_err(|error| ManifestError::InvalidAttribute(error.to_string()))?;
                        let local = attr.key.local_name();
                        let key = std::str::from_utf8(local.as_ref())?;
                        let value = attr
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map_err(ManifestError::Xml)?
                            .to_string();
                        match key {
                            "Id" => dependency_id = Some(value),
                            "Name" => dependency_name = Some(value),
                            "Publisher" => dependency_publisher = Some(value),
                            // Current NAVX manifests use MinVersion. Accept
                            // Version as well for older third-party packages.
                            "MinVersion" | "Version" => dependency_version = Some(value),
                            _ => {}
                        }
                    }
                    dependencies.push(ManifestDependency {
                        app_id: dependency_id.ok_or_else(|| {
                            ManifestError::MissingElement("Dependency/@Id".into())
                        })?,
                        name: dependency_name.ok_or_else(|| {
                            ManifestError::MissingElement("Dependency/@Name".into())
                        })?,
                        publisher: dependency_publisher.ok_or_else(|| {
                            ManifestError::MissingElement("Dependency/@Publisher".into())
                        })?,
                        min_version: dependency_version.ok_or_else(|| {
                            ManifestError::MissingElement("Dependency/@MinVersion".into())
                        })?,
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(ManifestError::Xml(e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(NavxManifest {
        app_id: app_id.ok_or_else(|| ManifestError::MissingElement("App/@Id".into()))?,
        name: name.ok_or_else(|| ManifestError::MissingElement("App/@Name".into()))?,
        publisher: publisher
            .ok_or_else(|| ManifestError::MissingElement("App/@Publisher".into()))?,
        version: version.ok_or_else(|| ManifestError::MissingElement("App/@Version".into()))?,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_manifest() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="urn:microsoft-dynamics-nav/package/v1">
  <App Id="63ca2fa4-4f03-4f2b-a480-172fef340d3f"
       Name="Base Application"
       Publisher="Microsoft"
       Version="24.0.12345.0"
       Brief="Base Application"
       Description="Base Application"
       CompatibilityId="24.0.0.0"
       PrivacyStatement=""
       EULA=""
       Url=""
       Logo=""
       Platform="24.0.0.0"
       Application="24.0.0.0"
       Runtime="14.0"
       Target="Cloud"
       ShowMyCode="Never" />
</Package>"#;

        let m = parse_manifest(xml.as_bytes()).unwrap();
        assert_eq!(m.app_id, "63ca2fa4-4f03-4f2b-a480-172fef340d3f");
        assert_eq!(m.name, "Base Application");
        assert_eq!(m.publisher, "Microsoft");
        assert_eq!(m.version, "24.0.12345.0");
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn parse_manifest_with_start_end_tags() {
        // Some manifests use <App ...> ... </App> instead of <App ... />
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Package>
  <App Id="abc-123" Name="Test App" Publisher="Test" Version="1.0.0.0">
    <Dependencies />
  </App>
</Package>"#;

        let m = parse_manifest(xml.as_bytes()).unwrap();
        assert_eq!(m.app_id, "abc-123");
        assert_eq!(m.name, "Test App");
    }

    #[test]
    fn parses_dependency_identity_and_minimum_version() {
        let xml = r#"<Package>
  <App Id="app" Name="App" Publisher="Vendor" Version="1.0.0.0" />
  <Dependencies>
    <Dependency Id="dep" Name="Shared &amp; Safe" Publisher="Other"
                MinVersion="2.3.0.0" CompatibilityId="0.0.0.0" />
  </Dependencies>
</Package>"#;
        let manifest = parse_manifest(xml.as_bytes()).expect("manifest");
        assert_eq!(
            manifest.dependencies,
            vec![ManifestDependency {
                app_id: "dep".to_string(),
                name: "Shared & Safe".to_string(),
                publisher: "Other".to_string(),
                min_version: "2.3.0.0".to_string(),
            }]
        );
    }

    #[test]
    fn malformed_dependency_fails_instead_of_disappearing() {
        let xml = r#"<Package>
  <App Id="app" Name="App" Publisher="Vendor" Version="1.0.0.0" />
  <Dependencies>
    <Dependency Id="dep" Name="Shared" MinVersion="1.0.0.0" />
  </Dependencies>
</Package>"#;
        let error = parse_manifest(xml.as_bytes()).expect_err("missing publisher must fail");
        assert!(error.to_string().contains("Dependency/@Publisher"));
    }

    #[test]
    fn parse_manifest_missing_id() {
        let xml = r#"<Package><App Name="X" Publisher="Y" Version="1.0" /></Package>"#;
        let err = parse_manifest(xml.as_bytes()).unwrap_err();
        assert!(matches!(err, ManifestError::MissingElement(_)));
    }
}
