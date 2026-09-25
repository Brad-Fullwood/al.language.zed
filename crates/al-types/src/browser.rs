//! Open a web URL in the user's browser.
//!
//! BC sign-in and the debugger's web client both open URLs whose query strings
//! carry `&`. On Windows, `cmd /c start "" <url>` re-parses the command line and
//! treats each `&` as a command separator, so the browser opened the truncated
//! URL and `cmd` then ran the rest as commands. The Windows opener here is
//! `rundll32 url.dll,FileProtocolHandler`, which hands the URL to the shell
//! without a command interpreter in between.

use std::process::{Command, Stdio};

/// Why a URL was not handed to the browser.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpenUrlError {
    /// Only `http://` and `https://` URLs are opened. A URL from a server
    /// response could otherwise name a `file:` path or a registered protocol
    /// handler.
    #[error("refusing to open a URL that is not http or https: {0}")]
    UnsupportedScheme(String),
    /// Whitespace, quotes and control characters are never valid in a URL and
    /// would change how the opener's command line splits.
    #[error("refusing to open a URL containing whitespace, quotes or control characters")]
    UnsafeCharacter,
    /// No opener is known for this platform.
    #[error("no browser opener is known for this platform")]
    UnsupportedPlatform,
    /// The opener process could not be started.
    #[error("could not start the browser opener `{program}`: {message}")]
    Spawn { program: String, message: String },
}

/// The platforms [`opener_command`] knows how to open a URL on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// Check that `url` is an `http` or `https` URL with nothing in it that a
/// command line could split on.
pub fn validate_browser_url(url: &str) -> Result<(), OpenUrlError> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        let scheme = url.split(':').next().unwrap_or_default();
        return Err(OpenUrlError::UnsupportedScheme(scheme.to_string()));
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '"')
    {
        return Err(OpenUrlError::UnsafeCharacter);
    }
    Ok(())
}

/// The program and arguments that open `url` on `platform`, after validating
/// the URL.
pub fn opener_command(
    url: &str,
    platform: Platform,
) -> Result<(&'static str, Vec<String>), OpenUrlError> {
    validate_browser_url(url)?;
    match platform {
        Platform::Linux => Ok(("xdg-open", vec![url.to_string()])),
        Platform::MacOs => Ok(("open", vec![url.to_string()])),
        Platform::Windows => Ok((
            "rundll32.exe",
            vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()],
        )),
        Platform::Other => Err(OpenUrlError::UnsupportedPlatform),
    }
}

/// Open `url` in the default browser. The opener runs detached with its
/// standard streams discarded; success means it started, not that a page
/// loaded.
pub fn open_in_browser(url: &str) -> Result<(), OpenUrlError> {
    let (program, args) = opener_command(url, Platform::current())?;
    Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| OpenUrlError::Spawn {
            program: program.to_string(),
            message: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTH_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=X&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A8080";

    #[test]
    fn windows_opens_a_query_string_without_a_command_interpreter() {
        let (program, args) = opener_command(AUTH_URL, Platform::Windows).unwrap();
        assert_eq!(program, "rundll32.exe");
        assert_eq!(args, vec!["url.dll,FileProtocolHandler", AUTH_URL]);
        assert!(
            !args.iter().any(|arg| arg == "/c"),
            "cmd.exe splits the URL on `&`"
        );
    }

    #[test]
    fn unix_openers_take_the_url_as_one_argument() {
        assert_eq!(
            opener_command(AUTH_URL, Platform::Linux).unwrap(),
            ("xdg-open", vec![AUTH_URL.to_string()])
        );
        assert_eq!(
            opener_command(AUTH_URL, Platform::MacOs).unwrap(),
            ("open", vec![AUTH_URL.to_string()])
        );
    }

    #[test]
    fn http_and_mixed_case_schemes_are_accepted() {
        assert!(validate_browser_url("http://bc-server:8080/BC/?tenant=default").is_ok());
        assert!(validate_browser_url("HTTPS://example.com").is_ok());
    }

    #[test]
    fn other_schemes_are_refused() {
        for url in [
            "file:///C:/Windows/System32/calc.exe",
            "ms-settings:privacy",
            "javascript:alert(1)",
            "",
            "https:",
        ] {
            assert!(
                matches!(
                    validate_browser_url(url),
                    Err(OpenUrlError::UnsupportedScheme(_))
                ),
                "{url:?} must be refused"
            );
        }
    }

    #[test]
    fn characters_that_split_a_command_line_are_refused() {
        for url in [
            "https://example.com/a b",
            "https://example.com/\"&calc",
            "https://example.com/\n",
            "https://example.com/\t",
        ] {
            assert_eq!(
                validate_browser_url(url),
                Err(OpenUrlError::UnsafeCharacter),
                "{url:?} must be refused"
            );
        }
    }

    #[test]
    fn unknown_platforms_report_it() {
        assert_eq!(
            opener_command(AUTH_URL, Platform::Other),
            Err(OpenUrlError::UnsupportedPlatform)
        );
    }
}
