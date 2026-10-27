//! Call shapes that depend on the server's debugger version.
//!
//! Microsoft reads `ServerInfo.DebuggerVersion` (ServerInfo.cs:11-12) from
//! `GET dev/metadata` and branches on its major part. The branches this client
//! reaches are listed here with their source lines.

use serde_json::{json, Value};

use super::session_config::BcDebugConfig;

/// First debugger version that has `Attach` (DebugAdapterAttachRequestHandler.cs:33-36).
pub(super) const ATTACH_MIN_MAJOR: u32 = 3;

/// Major part of `debuggerVersion` in the `dev/metadata` body. Keys are read
/// case-insensitively because Newtonsoft binds them that way. The value is a
/// version string such as `"26.0.0.0"` (`VersionConverter`, ServerInfo.cs:11).
pub(super) fn debugger_major_from_metadata(metadata: &Value) -> Option<u32> {
    let (_, value) = metadata
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("debuggerVersion"))?;
    value.as_str()?.split('.').next()?.trim().parse().ok()
}

/// `ConfigurationDone` takes no arguments for major 1 or lower and a version
/// `None` (HubBasedDebuggerService.cs:264-269).
pub(super) fn configuration_done_takes_options(major: Option<u32>) -> bool {
    major.is_none_or(|major| major > 1)
}

/// `GetWatchNode` has the third `WatchOption` argument from major 4
/// (HubBasedDebuggerService.cs:349-353).
pub(super) fn get_watch_node_takes_option(major: Option<u32>) -> bool {
    major.is_none_or(|major| major >= 4)
}

/// `DebugOptions` as Microsoft normalises them in `SetDebuggerStateAndValidateVersion`
/// (DebugAdapterStartRequestHandler.cs:69-119). Below major 5 the server knows
/// only the legacy booleans, so those are set. From major 5 the behaviour
/// enums carry the setting and the legacy booleans stay null. The behaviour
/// values are sent as configured in both cases, the handler only logs when a
/// version cannot honour `ExcludeTry` or `ExcludeTemporary`.
/// `SkipSystemTriggers` is fixed true (DebugOptions.cs default).
pub(super) fn debug_options(config: &BcDebugConfig, major: Option<u32>) -> Value {
    let legacy = major.is_some_and(|major| major < 5);
    let (break_on_error, break_on_record_write) = if legacy {
        (
            json!(config.break_on_error.enabled()),
            json!(config.break_on_record_write.enabled()),
        )
    } else {
        (Value::Null, Value::Null)
    };
    json!({
        "BreakOnError": break_on_error,
        "BreakOnErrorBehaviour": config.break_on_error.wire_value(),
        "BreakOnRecordWrite": break_on_record_write,
        "BreakOnRecordWriteBehaviour": config.break_on_record_write.wire_value(),
        "SkipSystemTriggers": true,
        "EnableSqlInformationDebugger": config.enable_sql_information_debugger,
        "EnableLongRunningSqlStatements": config.enable_long_running_sql_statements,
        "LongRunningSqlStatementsThreshold": config.long_running_sql_statements_threshold,
        "NumberOfSqlStatements": config.number_of_sql_statements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // fixture source: Deployment/Microsoft.Dynamics.Nav.Deployment.ApiClients/ServerInfo.cs:9-12
    // (VersionConverter properties RuntimeVersion, WebApiVersion, DebuggerVersion as version strings)
    #[test]
    fn major_is_read_from_debugger_version_case_insensitively() {
        let body = json!({"RuntimeVersion": "16.0.0.0", "WebApiVersion": "2.0.0.0", "DebuggerVersion": "9.0.0.0"});
        assert_eq!(debugger_major_from_metadata(&body), Some(9));
        assert_eq!(
            debugger_major_from_metadata(&json!({"debuggerVersion": "4.1"})),
            Some(4)
        );
        assert_eq!(debugger_major_from_metadata(&json!({})), None);
        assert_eq!(
            debugger_major_from_metadata(&json!({"debuggerVersion": "x"})),
            None
        );
    }

    #[test]
    fn call_shapes_switch_at_the_decompiled_majors() {
        // HubBasedDebuggerService.cs:264 (<=1 no args) and :349 (<4 two args).
        assert!(!configuration_done_takes_options(Some(1)));
        assert!(configuration_done_takes_options(Some(2)));
        assert!(!get_watch_node_takes_option(Some(3)));
        assert!(get_watch_node_takes_option(Some(4)));
    }

    #[test]
    fn debug_options_use_legacy_booleans_below_major_5_only() {
        let config = BcDebugConfig::default();
        let old = debug_options(&config, Some(4));
        assert!(old["BreakOnError"].is_boolean());
        assert!(old["BreakOnRecordWrite"].is_boolean());
        let new = debug_options(&config, Some(5));
        assert!(new["BreakOnError"].is_null());
        assert!(new["BreakOnRecordWrite"].is_null());
        assert!(new["BreakOnErrorBehaviour"].is_number());
        assert_eq!(new["SkipSystemTriggers"], true);
    }
}
