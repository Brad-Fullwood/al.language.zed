//! AL project scaffolding.
//!
//! Creates new AL projects from templates with standard file structure:
//! - `app.json` — project manifest
//! - `.gitignore` — AL-specific ignores
//! - `.zed/debug.json` — debug/launch configuration
//! - `src/` — source directory

use std::path::Path;

use serde::Serialize;

/// Configuration for scaffolding a new AL project.
#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    pub name: String,
    pub publisher: String,
    pub id: String,
    pub version: String,
    pub runtime: String,
    pub target: String,
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
        }
    }
}

/// Result of scaffolding operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaffoldResult {
    pub project_dir: String,
    pub files_created: Vec<String>,
}

/// Create a new AL project at the given path.
///
/// Creates the directory if it doesn't exist, generates standard files.
/// Returns error if the directory already contains an app.json.
pub fn create_project(dir: &Path, config: &ScaffoldConfig) -> Result<ScaffoldResult, String> {
    // Check for existing project
    if dir.join("app.json").exists() {
        return Err(format!(
            "Directory already contains an AL project: {}",
            dir.display()
        ));
    }

    // Create directories
    std::fs::create_dir_all(dir.join("src"))
        .map_err(|e| format!("Failed to create project directory: {e}"))?;
    std::fs::create_dir_all(dir.join(".zed"))
        .map_err(|e| format!("Failed to create .zed directory: {e}"))?;

    let mut files = Vec::new();

    // app.json — propagate serialization error rather than silently writing empty file
    let app_json = generate_app_json(config)?;
    std::fs::write(dir.join("app.json"), &app_json)
        .map_err(|e| format!("Failed to write app.json: {e}"))?;
    files.push("app.json".to_string());

    // .gitignore
    let gitignore = generate_gitignore();
    std::fs::write(dir.join(".gitignore"), &gitignore)
        .map_err(|e| format!("Failed to write .gitignore: {e}"))?;
    files.push(".gitignore".to_string());

    // .zed/debug.json — propagate serialization error rather than silently writing empty file
    let debug_json = generate_debug_json()?;
    std::fs::write(dir.join(".zed/debug.json"), &debug_json)
        .map_err(|e| format!("Failed to write .zed/debug.json: {e}"))?;
    files.push(".zed/debug.json".to_string());

    // src/HelloWorld.Codeunit.al (starter file)
    let starter = generate_starter_codeunit(config);
    let starter_name = "src/HelloWorld.Codeunit.al";
    std::fs::write(dir.join(starter_name), &starter)
        .map_err(|e| format!("Failed to write starter codeunit: {e}"))?;
    files.push(starter_name.to_string());

    Ok(ScaffoldResult {
        project_dir: dir.display().to_string(),
        files_created: files,
    })
}

fn generate_app_json(config: &ScaffoldConfig) -> Result<String, String> {
    serde_json::to_string_pretty(&serde_json::json!({
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
        "target": config.target,
        "features": ["NoImplicitWith"]
    }))
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
            "useMcpServerForDebugging": true,
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
            "useMcpServerForDebugging": true,
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
            "tenant": "default",
            "useMcpServerForDebugging": true
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
            "breakOnNext": "WebServiceClient",
            "useMcpServerForDebugging": true
        }
    ]))
    .map_err(|e| format!("Failed to serialize debug.json: {e}"))
}

fn generate_starter_codeunit(config: &ScaffoldConfig) -> String {
    format!(
        r#"codeunit 50100 "Hello World"
{{
    trigger OnRun()
    begin
        Message('Hello from {}!');
    end;
}}
"#,
        config.name
    )
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
}
