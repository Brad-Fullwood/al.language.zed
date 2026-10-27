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
    state.configured = configured.filter(|n| *n > 0);
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
pub fn index_pool() -> Arc<rayon::ThreadPool> {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let want = resolved(&state);
    if let Some((have, pool)) = &state.pool {
        if *have == want {
            return Arc::clone(pool);
        }
    }
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(want)
            .thread_name(|i| format!("al-index-{i}"))
            .start_handler(|_| lower_priority())
            .build()
            .expect("index thread pool"),
    );
    state.pool = Some((want, Arc::clone(&pool)));
    pool
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
    #[cfg(target_os = "linux")]
    fn workers_run_at_nice_ten_and_respect_the_override() {
        set_index_threads(Some(3));
        let pool = index_pool();
        assert_eq!(pool.current_num_threads(), 3);
        let nice = pool.install(|| unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::getpriority(libc::PRIO_PROCESS, tid)
        });
        assert_eq!(nice, INDEX_NICE);
        set_index_threads(None);
    }
}
