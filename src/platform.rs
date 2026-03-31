use std::collections::HashMap;

/// Detected host platform
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Platform {
    Linux,
    MacOS,
    Windows,
}

impl Platform {
    /// Get the binary subdirectory name for this platform
    pub fn bin_dir(&self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::MacOS => "darwin",
            Platform::Windows => "win32",
        }
    }

    /// Get the binary filename for this platform
    pub fn binary_name(&self) -> &'static str {
        match self {
            Platform::Windows => "al-lsp.exe",
            _ => "al-lsp",
        }
    }

    /// Get the Zed extensions base path for this platform
    pub fn extensions_base(&self, home: &str) -> String {
        match self {
            Platform::Linux => format!("{}/.local/share/zed/extensions/installed", home),
            Platform::MacOS => format!(
                "{}/Library/Application Support/Zed/extensions/installed",
                home
            ),
            Platform::Windows => {
                // On Windows, HOME might be set but we should use APPDATA if available
                // For now, use a Windows-style path
                format!("{}/AppData/Roaming/Zed/extensions/installed", home)
            }
        }
    }
}

/// Detect the host platform from environment variables
///
/// Strategy:
/// 1. Primary: Check OSTYPE environment variable (set by bash/zsh)
/// 2. Fallback 1: Check if HOME looks like a Windows path (starts with drive letter)
/// 3. Fallback 2: Check if macOS system path exists
/// 4. Default: Linux (development environment)
pub fn detect_platform(env_map: &HashMap<String, String>) -> Platform {
    if let Some(ostype) = env_map.get("OSTYPE") {
        let ostype_lower = ostype.to_lowercase();
        if ostype_lower.starts_with("darwin") {
            return Platform::MacOS;
        } else if ostype_lower.starts_with("msys")
            || ostype_lower.starts_with("cygwin")
            || ostype_lower.starts_with("win")
        {
            return Platform::Windows;
        } else if ostype_lower.starts_with("linux") {
            return Platform::Linux;
        }
    }

    // USERPROFILE and HOMEDRIVE are set by Windows natively (even without MSYS/Cygwin).
    // Check them before the HOME drive-letter heuristic.
    if env_map.contains_key("USERPROFILE") || env_map.contains_key("HOMEDRIVE") {
        return Platform::Windows;
    }

    if let Some(home) = env_map.get("HOME") {
        if home.len() >= 2 {
            let first_char = home.chars().next().unwrap_or(' ');
            let second_char = home.chars().nth(1).unwrap_or(' ');
            if first_char.is_ascii_alphabetic() && second_char == ':' {
                return Platform::Windows;
            }
        }
    }

    // Path::exists() doesn't work in WASM — use std::fs::metadata()
    if std::fs::metadata("/System/Library").is_ok() {
        return Platform::MacOS;
    }

    Platform::Linux
}
