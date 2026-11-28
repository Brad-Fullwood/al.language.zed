//! Bounded, low priority thread pool for building symbol indexes.
//!
//! Checked first: rayon's global pool uses every core at normal priority, which starves the
//! editor during a cold index of Base Application. `rayon::ThreadPoolBuilder` takes a thread
//! count and a `start_handler`, so the pool is built from that and each worker lowers its own
//! priority with `libc::setpriority` (Linux) or a utility QoS class (macOS). No other
//! standard facility sets per-pool priority.

use std::sync::{Arc, Mutex};

/// Nice value for index workers.
pub const INDEX_NICE: i32 = 10;
/// Upper bound of the default thread count.
pub const DEFAULT_MAX_THREADS: usize = 4;
/// Upper bound of a configured thread count. `al.indexThreads` above this is
/// lowered to it.
pub const MAX_INDEX_THREADS: usize = 64;

struct State {
    configured: Option<usize>,
    pool: Option<(usize, Arc<rayon::ThreadPool>)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    configured: None,
    pool: None,
});

/// Default thread count: half the cores, at least 2, at most 4.
pub fn default_threads(available: usize) -> usize {
    (available / 2).clamp(2, DEFAULT_MAX_THREADS)
}

/// Sets the `al.indexThreads` override (`None` restores the default). Takes effect for the
/// next index build, builds already running keep their pool.
pub fn set_index_threads(configured: Option<usize>) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.configured = configured
        .filter(|n| *n > 0)
        .map(|n| n.min(MAX_INDEX_THREADS));
}

/// Thread count the pool uses right now.
pub fn index_threads() -> usize {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    resolved(&state)
}

fn resolved(state: &State) -> usize {
    state.configured.unwrap_or_else(|| {
        default_threads(std::thread::available_parallelism().map_or(2, |n| n.get()))
    })
}

/// The shared index pool, rebuilt when the configured thread count changed.
///
/// Returns `None` when no pool can start, and the caller indexes on its own
/// thread. Only a built pool is cached, so a `None` is retried on the next call.
pub fn index_pool() -> Option<Arc<rayon::ThreadPool>> {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let want = resolved(&state);
    if let Some((have, pool)) = &state.pool {
        if *have == want {
            return Some(Arc::clone(pool));
        }
    }
    let pool = Arc::new(build_pool(want)?);
    state.pool = Some((want, Arc::clone(&pool)));
    Some(pool)
}

/// A pool of `threads` workers, or `None` when no pool can start.
/// See [`build_pool_with`].
fn build_pool(threads: usize) -> Option<rayon::ThreadPool> {
    build_pool_with(threads, |n| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("al-index-{i}"))
            .start_handler(|_| lower_priority())
            .build()
    })
}

/// Tries `threads`, then the default size, then one thread, and returns the
/// first pool that starts. Each refusal logs a warning. When no size starts,
/// the last warning says indexing runs on the calling thread and this returns
/// `None`.
fn build_pool_with(
    threads: usize,
    build: impl Fn(usize) -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>,
) -> Option<rayon::ThreadPool> {
    let fallback = default_threads(std::thread::available_parallelism().map_or(2, |n| n.get()));
    for size in [threads, fallback, 1] {
        match build(size) {
            Ok(pool) => return Some(pool),
            Err(error) => {
                tracing::warn!(threads = size, %error, "index thread pool could not start");
            }
        }
    }
    tracing::warn!("no index thread pool could start, indexing runs on the calling thread");
    None
}

#[cfg(target_os = "linux")]
fn lower_priority() {
    // On Linux a thread id is accepted as a "process" id by setpriority, so this
    // affects only the calling thread.
    // SAFETY: plain syscalls with no pointers.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, INDEX_NICE);
    }
}

#[cfg(target_os = "macos")]
fn lower_priority() {
    // SAFETY: affects only the calling thread.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn lower_priority() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_half_the_cores_between_two_and_four() {
        assert_eq!(default_threads(1), 2);
        assert_eq!(default_threads(4), 2);
        assert_eq!(default_threads(8), 4);
        assert_eq!(default_threads(12), 4);
        assert_eq!(default_threads(64), 4);
    }

    #[test]
    fn a_clamped_thread_count_builds() {
        let pool = build_pool(100_000usize.min(MAX_INDEX_THREADS)).expect("a clamped pool builds");
        assert_eq!(pool.current_num_threads(), MAX_INDEX_THREADS);
    }

    #[test]
    fn pool_refused_at_every_size_returns_none() {
        let attempts = std::cell::Cell::new(0);
        let pool = build_pool_with(4, |n| {
            attempts.set(attempts.get() + 1);
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .spawn_handler(|_| Err(std::io::Error::other("refused")))
                .build()
        });
        assert!(pool.is_none());
        assert_eq!(attempts.get(), 3, "requested, default and one-thread sizes");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn workers_run_at_nice_ten_and_respect_the_override() {
        set_index_threads(Some(3));
        let pool = index_pool().expect("the index pool starts in tests");
        assert_eq!(pool.current_num_threads(), 3);
        let nice = pool.install(|| unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::getpriority(libc::PRIO_PROCESS, tid)
        });
        assert_eq!(nice, INDEX_NICE);
        set_index_threads(None);
    }
}
