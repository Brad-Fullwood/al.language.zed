//! What this process is costing the machine, for `status` and `diag`.
//!
//! The per-structure byte totals in `memory_stats` count allocations the
//! workspace owns. They do not account for what the allocator holds, so a
//! daemon can sit at gigabytes of resident memory while every total it reports
//! is small. These two numbers are the view the operating system has, which is
//! the one a developer deciding whether to restart a daemon needs.

/// Current and peak resident set size in bytes, as far as this platform can
/// report them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ResidentMemory {
    pub(crate) current_bytes: Option<u64>,
    pub(crate) peak_bytes: Option<u64>,
}

impl ResidentMemory {
    pub(crate) fn read() -> Self {
        Self {
            current_bytes: current_resident_bytes(),
            peak_bytes: peak_resident_bytes(),
        }
    }

    pub(crate) fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "residentBytes": self.current_bytes,
            "peakResidentBytes": self.peak_bytes,
        })
    }
}

/// Linux publishes the current resident size in `/proc/self/status`, already
/// in kilobytes, so this needs no page size.
#[cfg(target_os = "linux")]
fn current_resident_bytes() -> Option<u64> {
    parse_status_kb(
        &std::fs::read_to_string("/proc/self/status").ok()?,
        "VmRSS:",
    )
}

/// No portable way to read the *current* resident size off Linux without
/// platform APIs this crate does not otherwise need. The peak below still
/// answers "did this daemon grow to gigabytes".
#[cfg(not(target_os = "linux"))]
fn current_resident_bytes() -> Option<u64> {
    None
}

/// The peak this process has reached, as the kernel last recorded it.
///
/// The kernel updates `ru_maxrss` at its own points rather than on every page
/// fault, so a reading can sit just below the current size from
/// `/proc/self/status`. Treat the two as independent samples, not as a bound.
#[cfg(unix)]
fn peak_resident_bytes() -> Option<u64> {
    // SAFETY: `getrusage` writes into a caller-owned struct, and `zeroed` is a
    // valid `rusage` (all its members are integers).
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    let max_rss = u64::try_from(usage.ru_maxrss).ok()?;
    // `ru_maxrss` is kilobytes on Linux and bytes on macOS.
    #[cfg(target_os = "linux")]
    return Some(max_rss * 1024);
    #[cfg(not(target_os = "linux"))]
    return Some(max_rss);
}

#[cfg(not(unix))]
fn peak_resident_bytes() -> Option<u64> {
    None
}

/// The value of a `/proc/self/status` line like `VmRSS:  2971648 kB`, in bytes.
#[cfg(target_os = "linux")]
fn parse_status_kb(status: &str, key: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with(key))?;
    let kb: u64 = line
        .strip_prefix(key)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_status_line_reads_as_bytes() {
        let status = "Name:\tal-lsp\nVmHWM:\t 3053124 kB\nVmRSS:\t 2971648 kB\n";
        assert_eq!(parse_status_kb(status, "VmRSS:"), Some(2971648 * 1024));
        assert_eq!(parse_status_kb(status, "VmHWM:"), Some(3053124 * 1024));
        assert_eq!(parse_status_kb(status, "VmNope:"), None);
    }

    /// The number that would have shown the 2.9 GB daemon for what it was.
    #[test]
    fn this_process_reports_a_plausible_footprint() {
        let memory = ResidentMemory::read();
        let json = memory.to_json();
        assert!(json.get("residentBytes").is_some());
        assert!(json.get("peakResidentBytes").is_some());

        if let Some(bytes) = memory.current_bytes {
            assert!(
                bytes > 1024 * 1024,
                "a running test process holds more than a megabyte, got {bytes}"
            );
        }
        if let Some(bytes) = memory.peak_bytes {
            assert!(
                bytes > 1024 * 1024,
                "the peak cannot be under a megabyte either, got {bytes}"
            );
        }
    }
}
