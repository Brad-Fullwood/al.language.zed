//! AL project discovery.
//!
//! Re-exports from `al_protocol`. Tests live in `al_protocol::project`.

pub use al_protocol::{AlProject, AppDependency, AppManifest, NuGetFeed};
pub use al_protocol::project::{find_project, home_dir, nuget_feeds};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_find_project_with_temp_dir() {
        let tmp = tempdir();
        let project_dir = tmp.join("my-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("app.json"), serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "Test", "publisher": "Test", "version": "1.0.0.0"
        }).to_string()).unwrap();

        let project = find_project(&project_dir).unwrap();
        assert_eq!(project.root, project_dir);
        assert_eq!(project.app_json.name, "Test");
    }

    #[test]
    fn test_nuget_feeds_returns_three() {
        assert_eq!(nuget_feeds().len(), 3);
    }

    #[test]
    fn test_all_dependencies_includes_implicit() {
        let project = AlProject {
            root: PathBuf::from("/tmp/fake"),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".into(),
                name: "Test".into(), publisher: "Test".into(), version: "1.0.0.0".into(),
                dependencies: vec![], application: Some("25.0.0.0".into()),
                platform: Some("25.0.0.0".into()), runtime: None,
            },
            packages_dir: PathBuf::from("/tmp/fake/.alpackages"),
            packages: vec![], server_configs: vec![],
        };
        assert!(project.all_dependencies().len() >= 5);
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("al-core-project-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
