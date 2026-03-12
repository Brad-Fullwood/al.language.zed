use std::fs;
use zed_extension_api::{self as zed, lsp::CompletionKind, lsp::SymbolKind, settings::LspSettings, CodeLabel, CodeLabelSpan, Result};

struct AlExtension {
    cached_binary_path: Option<String>,
}

const BINARY_NAME: &str = "al-lsp";

impl AlExtension {
    /// Find the al-lsp binary.
    ///
    /// Search order:
    /// 1. Cached path from a previous successful lookup
    /// 2. User-configured path in Zed settings
    /// 3. System PATH via `worktree.which()`
    /// 4. Previously downloaded binary in the extension work directory
    /// 5. Download from GitHub releases
    fn find_binary(&mut self, worktree: &zed::Worktree) -> Result<String> {
        if let Some(ref path) = self.cached_binary_path {
            return Ok(path.clone());
        }

        // Check PATH
        if let Some(path) = worktree.which(BINARY_NAME) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Check extension work directory for previously downloaded binary
        let (platform, arch) = zed::current_platform();
        let binary_name = match platform {
            zed::Os::Windows => "al-lsp.exe",
            _ => BINARY_NAME,
        };
        let version = env!("CARGO_PKG_VERSION");
        let install_dir = format!("al-lsp-{version}");
        let binary_path = format!("{install_dir}/{binary_name}");

        if fs::metadata(&binary_path).is_ok() {
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        // Download from GitHub releases
        let triple = format!("{}-{}", arch_str(arch), os_str(platform));
        let download_url = format!(
            "https://github.com/Brad-Fullwood/zed-al/releases/download/v{version}/al-lsp-{triple}.tar.gz"
        );

        match zed::download_file(&download_url, &install_dir, zed::DownloadedFileType::GzipTar) {
            Ok(()) => {
                zed::make_file_executable(&binary_path)?;
                remove_outdated_versions(&install_dir);
                self.cached_binary_path = Some(binary_path.clone());
                Ok(binary_path)
            }
            Err(download_err) => Err(format!(
                "Could not find `{BINARY_NAME}` on PATH and failed to download from \
                 {download_url}: {download_err}\n\n\
                 Install manually:\n  \
                 cargo install --git https://github.com/Brad-Fullwood/zed-al {BINARY_NAME}\n\n\
                 Or set the path in Zed settings:\n  \
                 {{\"lsp\": {{\"{BINARY_NAME}\": {{\"binary\": {{\"path\": \"/path/to/{BINARY_NAME}\"}}}}}}}}"
            )),
        }
    }
}

impl zed::Extension for AlExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;

        let binary_path = if let Some(path) =
            settings.binary.as_ref().and_then(|b| b.path.as_ref())
        {
            path.to_string()
        } else {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::CheckingForUpdate,
            );
            let binary = self.find_binary(worktree)?;
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::None,
            );
            binary
        };

        let args: Vec<String> = settings
            .binary
            .as_ref()
            .and_then(|b| b.arguments.as_ref())
            .cloned()
            .unwrap_or_default();

        Ok(zed::Command {
            command: binary_path,
            args,
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        Ok(settings.initialization_options)
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        Ok(settings.settings)
    }

    // ── Completion & Symbol Labels ────────────────────────────────

    fn label_for_completion(
        &self,
        _language_server_id: &zed::LanguageServerId,
        completion: zed::lsp::Completion,
    ) -> Option<CodeLabel> {
        let kind = completion.kind?;
        let label = &completion.label;
        let detail = completion.detail.as_deref();

        let (highlight, show_detail) = match kind {
            CompletionKind::Function | CompletionKind::Method | CompletionKind::Event => {
                ("function", true)
            }
            CompletionKind::Variable => ("variable", true),
            CompletionKind::Keyword => ("keyword", false),
            CompletionKind::Struct
            | CompletionKind::Class
            | CompletionKind::Module
            | CompletionKind::Enum
            | CompletionKind::Interface
            | CompletionKind::Reference => ("type", true),
            CompletionKind::Field | CompletionKind::Property => ("property", true),
            CompletionKind::EnumMember | CompletionKind::Constant => ("constant", false),
            CompletionKind::Snippet => ("keyword", false),
            _ => return None,
        };

        let mut spans = vec![CodeLabelSpan::literal(
            label.clone(),
            Some(highlight.to_string()),
        )];

        if show_detail {
            if let Some(d) = detail {
                if !d.is_empty() {
                    spans.push(CodeLabelSpan::literal(
                        format!("  {d}"),
                        Some("comment".to_string()),
                    ));
                }
            }
        }

        Some(CodeLabel {
            filter_range: (0..label.len()).into(),
            spans,
            code: String::new(),
        })
    }

    fn label_for_symbol(
        &self,
        _language_server_id: &zed::LanguageServerId,
        symbol: zed::lsp::Symbol,
    ) -> Option<CodeLabel> {
        let name = &symbol.name;

        let kind_keyword = match symbol.kind {
            SymbolKind::Struct => "table",
            SymbolKind::Class => "page",
            SymbolKind::Module => "codeunit",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::File => "report",
            SymbolKind::Function | SymbolKind::Method => "procedure",
            SymbolKind::Object => "xmlport",
            _ => return None,
        };

        Some(CodeLabel {
            filter_range: (0..(kind_keyword.len() + 1 + name.len())).into(),
            spans: vec![
                CodeLabelSpan::literal(kind_keyword, Some("keyword".to_string())),
                CodeLabelSpan::literal(format!(" {name}"), Some("type".to_string())),
            ],
            code: String::new(),
        })
    }

    // ── Debug Adapter Protocol ───────────────────────────────────

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        config: zed::DebugTaskDefinition,
        _user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> std::result::Result<zed::DebugAdapterBinary, String> {
        let binary_path = self.find_binary(worktree)?;

        let config_value: zed::serde_json::Value =
            zed::serde_json::from_str(&config.config).unwrap_or_default();

        let request_type = config_value
            .get("request")
            .and_then(|v| v.as_str())
            .unwrap_or("launch");

        let request = match request_type {
            "attach" => zed::StartDebuggingRequestArgumentsRequest::Attach,
            _ => zed::StartDebuggingRequestArgumentsRequest::Launch,
        };

        Ok(zed::DebugAdapterBinary {
            command: Some(binary_path),
            arguments: vec!["--dap".to_string()],
            envs: vec![],
            cwd: Some(worktree.root_path()),
            connection: None,
            request_args: zed::StartDebuggingRequestArguments {
                configuration: config.config,
                request,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: zed::serde_json::Value,
    ) -> std::result::Result<zed::StartDebuggingRequestArgumentsRequest, String> {
        let request = config
            .get("request")
            .and_then(|v| v.as_str())
            .unwrap_or("launch");

        match request {
            "attach" => Ok(zed::StartDebuggingRequestArgumentsRequest::Attach),
            _ => Ok(zed::StartDebuggingRequestArgumentsRequest::Launch),
        }
    }

    fn dap_config_to_scenario(
        &mut self,
        config: zed::DebugConfig,
    ) -> std::result::Result<zed::DebugScenario, String> {
        let mut cfg = zed::serde_json::Map::new();

        match &config.request {
            zed::DebugRequest::Launch(launch) => {
                cfg.insert(
                    "request".to_string(),
                    zed::serde_json::Value::String("launch".to_string()),
                );
                if !launch.program.is_empty() {
                    cfg.insert(
                        "server".to_string(),
                        zed::serde_json::Value::String(launch.program.clone()),
                    );
                }
                let args = &launch.args;
                if let Some(instance) = args.first() {
                    cfg.insert(
                        "serverInstance".to_string(),
                        zed::serde_json::Value::String(instance.clone()),
                    );
                }
                if let Some(tenant) = args.get(1) {
                    cfg.insert(
                        "tenant".to_string(),
                        zed::serde_json::Value::String(tenant.clone()),
                    );
                }
            }
            zed::DebugRequest::Attach(_) => {
                cfg.insert(
                    "request".to_string(),
                    zed::serde_json::Value::String("attach".to_string()),
                );
                cfg.insert(
                    "breakOnNext".to_string(),
                    zed::serde_json::Value::String("WebClient".to_string()),
                );
            }
        }

        let config_json = zed::serde_json::to_string(&cfg)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        Ok(zed::DebugScenario {
            label: config.label.clone(),
            adapter: "al".to_string(),
            build: None,
            config: config_json,
            tcp_connection: None,
        })
    }
}

zed::register_extension!(AlExtension);

fn os_str(os: zed::Os) -> &'static str {
    match os {
        zed::Os::Mac => "apple-darwin",
        zed::Os::Linux => "unknown-linux-gnu",
        zed::Os::Windows => "pc-windows-msvc",
    }
}

fn arch_str(arch: zed::Architecture) -> &'static str {
    match arch {
        zed::Architecture::Aarch64 => "aarch64",
        zed::Architecture::X8664 => "x86_64",
        zed::Architecture::X86 => "x86",
    }
}

/// Remove old al-lsp version directories after a successful download.
fn remove_outdated_versions(current_dir: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if name.starts_with("al-lsp-") && name != current_dir {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
