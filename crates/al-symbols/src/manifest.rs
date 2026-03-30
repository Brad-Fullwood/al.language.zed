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

/// Parsed NavxManifest.xml data.
#[derive(Debug, Clone)]
pub struct NavxManifest {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// Parse a NavxManifest.xml from bytes.
///
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
                        let val = std::str::from_utf8(&attr.value)?;
                        match key {
                            "Id" => app_id = Some(val.to_string()),
                            "Name" => name = Some(val.to_string()),
                            "Publisher" => publisher = Some(val.to_string()),
                            "Version" => version = Some(val.to_string()),
                            _ => {}
                        }
                    }
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
    fn parse_manifest_missing_id() {
        let xml = r#"<Package><App Name="X" Publisher="Y" Version="1.0" /></Package>"#;
        let err = parse_manifest(xml.as_bytes()).unwrap_err();
        assert!(matches!(err, ManifestError::MissingElement(_)));
    }
}
