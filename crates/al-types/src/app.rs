//! `app.json` dependency descriptor, shared by project discovery, the BC client
//! (dev-package URLs) and the symbol/NuGet layer.

/// An `app.json` dependency entry.
///
/// Fields use camelCase for JSON serialization to match the `app.json` format.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDependency {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}
