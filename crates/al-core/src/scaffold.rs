//! AL project scaffolding.
//!
//! Creates new AL projects from templates with standard file structure:
//! - `app.json` — project manifest
//! - `.gitignore` — AL-specific ignores
//! - `.zed/debug.json` — debug/launch configuration
//! - `src/` — source directory

use std::path::Path;
use std::str::FromStr;

use serde::Serialize;

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
            other => Err(format!(
                "Unknown template '{}'. Valid: default, pte, appsource, library, test, copilot, agent, api",
                other
            )),
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
    }
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
        let result = crate::syntax::parser::AlParser::parse_quick(source);
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
}
