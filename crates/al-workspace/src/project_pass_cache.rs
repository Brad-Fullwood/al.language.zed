//! Results of whole-project analysis passes, kept for one workspace
//! generation.
//!
//! A project pass reads every workspace file and every dependency package, so
//! its result changes only when the workspace generation, the call graph
//! invalidation revision or the dependency packages change. Some callers
//! invalidate the call graph without a new generation, for example after a
//! package load or a daemon file refresh.
//!
//! The editor asks for the same pass several times per generation: once per
//! opened file at start-up, then the call graph refresh and the project
//! republish after a typing pause. Without this cache each ask recomputed the
//! pass, and the start-up asks ran side by side, each with its own copy of the
//! resolved call graph.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::DependencyFingerprint;

/// The workspace generation and the call graph invalidation revision.
type Revision = (u64, u64);

struct Entry {
    revision: Revision,
    fingerprint: DependencyFingerprint,
    value: Arc<dyn Any + Send + Sync>,
}

#[derive(Default)]
pub(crate) struct ProjectPassCache {
    entries: Mutex<HashMap<&'static str, Entry>>,
    /// Held while a pass computes, so concurrent asks wait for the first one
    /// and then read its result.
    build_lock: Mutex<()>,
}

impl ProjectPassCache {
    fn lookup<T: Any + Send + Sync>(
        &self,
        key: &'static str,
        revision: Revision,
        fingerprint: &DependencyFingerprint,
    ) -> Option<Arc<T>> {
        // A poisoned lock only means a pass panicked. The map holds complete
        // entries, so it is still usable.
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries.get(key)?;
        if entry.revision != revision || &entry.fingerprint != fingerprint {
            return None;
        }
        Arc::clone(&entry.value).downcast::<T>().ok()
    }

    pub(crate) fn get_or_compute<T, E>(
        &self,
        key: &'static str,
        stamp: impl Fn() -> (Revision, DependencyFingerprint),
        compute: impl FnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E>
    where
        T: Any + Send + Sync,
    {
        let (revision, fingerprint) = stamp();
        if let Some(hit) = self.lookup(key, revision, &fingerprint) {
            return Ok(hit);
        }
        let _build = self
            .build_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (revision, fingerprint) = stamp();
        if let Some(hit) = self.lookup(key, revision, &fingerprint) {
            return Ok(hit);
        }
        let value = Arc::new(compute()?);
        // A result computed while the generation moved may mix two
        // generations. It is returned to this caller but not kept.
        if stamp() == (revision, fingerprint.clone()) {
            let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries.insert(
                key,
                Entry {
                    revision,
                    fingerprint,
                    value: Arc::clone(&value) as Arc<dyn Any + Send + Sync>,
                },
            );
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::Workspace;

    fn counted(workspace: &Workspace, calls: &AtomicUsize) -> Arc<u64> {
        workspace
            .cached_project_pass("test", || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(workspace.generation_revision())
            })
            .unwrap()
    }

    #[test]
    fn computes_once_per_generation() {
        let workspace = Workspace::new();
        let calls = AtomicUsize::new(0);
        assert_eq!(*counted(&workspace, &calls), *counted(&workspace, &calls));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        workspace.mark_generation_changed();
        counted(&workspace, &calls);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a new generation recomputes"
        );
    }

    #[test]
    fn call_graph_invalidation_recomputes() {
        let workspace = Workspace::new();
        let calls = AtomicUsize::new(0);
        counted(&workspace, &calls);
        workspace.invalidate_call_graph_only();
        counted(&workspace, &calls);
        workspace.invalidate_insight_graph();
        counted(&workspace, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn concurrent_callers_share_one_computation() {
        let workspace = Arc::new(Workspace::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..5)
            .map(|_| {
                let workspace = Arc::clone(&workspace);
                let calls = Arc::clone(&calls);
                std::thread::spawn(move || {
                    workspace
                        .cached_project_pass("test", || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            Ok::<_, ()>(())
                        })
                        .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn result_of_a_moved_generation_is_not_kept() {
        let workspace = Workspace::new();
        let calls = AtomicUsize::new(0);
        workspace
            .cached_project_pass("test", || {
                calls.fetch_add(1, Ordering::SeqCst);
                workspace.mark_generation_changed();
                Ok::<_, ()>(0u64)
            })
            .unwrap();
        counted(&workspace, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_computation_is_not_kept() {
        let workspace = Workspace::new();
        let failed: Result<Arc<u64>, &str> = workspace.cached_project_pass("test", || Err("no"));
        assert!(failed.is_err());
        let calls = AtomicUsize::new(0);
        counted(&workspace, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
