//! Shared AL launch configuration.
//!
//! DAP previously carried a second parser for `.zed/debug.json` and
//! `.vscode/launch.json`. That copy drifted from publishing, project discovery,
//! and daemon behavior, including swallowing malformed files. Keep one parser
//! and one failure policy for every consumer.

pub use al_bc::launch::{
    find_launch_config, AuthMethod, BcServerConfig as DapLaunchConfig, DebugConfigFile,
    EnvironmentType, LaunchConfigError,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_parser_is_available_to_dap_consumers() {
        let temp = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir_all(temp.path().join(".vscode")).expect("config directory");
        std::fs::write(
            temp.path().join(".vscode/launch.json"),
            r#"{
                "configurations": [{
                    "name": "Sandbox",
                    "type": "al",
                    "environmentType": "Sandbox"
                }]
            }"#,
        )
        .expect("launch file");

        let file = find_launch_config(temp.path())
            .expect("valid launch file")
            .expect("AL launch configuration");
        assert_eq!(file.configs.len(), 1);
        assert_eq!(file.configs[0].name, "Sandbox");
        assert_eq!(file.configs[0].environment_type, EnvironmentType::Sandbox);
    }

    #[test]
    fn malformed_shared_config_is_not_treated_as_absent() {
        let temp = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir_all(temp.path().join(".zed")).expect("config directory");
        std::fs::write(temp.path().join(".zed/debug.json"), "{not json").expect("launch file");

        let error = find_launch_config(temp.path()).expect_err("malformed config must fail");
        assert!(error.path.ends_with(".zed/debug.json"));
    }
}
