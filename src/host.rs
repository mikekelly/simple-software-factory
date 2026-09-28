//! The factory host's load for the status payload's `host` field (#632):
//! CPU, memory and disk use as percentages. Linux reads `/proc`; anywhere
//! else, or when a reading fails, a field is null.

use serde_json::{Value, json};
use std::path::Path;
use std::sync::Mutex;

/// The last `/proc/stat` CPU sample, so each status reports the CPU use
/// averaged since the one before it (the clients' refresh interval).
static LAST_CPU: Mutex<Option<CpuSample>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    pub total: u64,
    pub idle: u64,
}

/// The aggregate `cpu` line of `/proc/stat`: every jiffy, and the idle ones
/// (idle plus iowait).
pub fn parse_proc_stat(text: &str) -> Option<CpuSample> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    if fields.len() < 4 {
        return None;
    }
    // guest and guest_nice (fields 9, 10) are already counted in user and nice.
    let total = fields.iter().take(8).sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some(CpuSample { total, idle })
}

/// The busy share between two samples, in percent; `None` when no time
/// passed between them.
pub fn cpu_percent(before: CpuSample, now: CpuSample) -> Option<f64> {
    let total = now.total.checked_sub(before.total)?;
    let idle = now.idle.checked_sub(before.idle)?;
    if total == 0 {
        return None;
    }
    Some(round(
        100.0 * total.saturating_sub(idle) as f64 / total as f64,
    ))
}

/// Memory in use, from `/proc/meminfo`: MemTotal less MemAvailable.
pub fn mem_percent(meminfo: &str) -> Option<f64> {
    let m = crate::vm::parse_meminfo(meminfo)?;
    if m.total_kib == 0 {
        return None;
    }
    let used = m.total_kib.saturating_sub(m.available_kib);
    Some(round(100.0 * used as f64 / m.total_kib as f64))
}

fn round(p: f64) -> f64 {
    (p * 10.0).round() / 10.0
}

fn cpu_now() -> Option<f64> {
    let now = parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)?;
    let mut last = LAST_CPU.lock().ok()?;
    let before = last.replace(now)?;
    cpu_percent(before, now)
}

fn disk_percent(path: &Path) -> Option<f64> {
    let d = crate::vm::disk_use(path).ok()?;
    // As df counts it: used over used plus what an unprivileged user may take.
    let usable = d.used_bytes + d.avail_bytes;
    (usable > 0).then(|| round(100.0 * d.used_bytes as f64 / usable as f64))
}

/// The `host` object: `cpu_percent` (null on the first status after start,
/// having nothing to average against), `mem_percent` and `disk_percent` of
/// the filesystem holding the factory's state.
pub fn host_json() -> Value {
    if !cfg!(target_os = "linux") {
        return json!({"cpu_percent": null, "mem_percent": null, "disk_percent": null});
    }
    let mem = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| mem_percent(&t));
    json!({
        "cpu_percent": cpu_now(),
        "mem_percent": mem,
        "disk_percent": disk_percent(&crate::config::state_dir()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_aggregate_cpu_line() {
        let s =
            parse_proc_stat("cpu  100 5 50 800 20 1 2 3 7 0\ncpu0 1 1 1 1 1 1 1 1 0 0\nintr 1\n")
                .unwrap();
        assert_eq!(
            s,
            CpuSample {
                total: 981,
                idle: 820
            }
        );
        assert!(parse_proc_stat("cpu0 1 2 3 4\n").is_none());
        assert!(parse_proc_stat("cpu  1 x 3 4\n").is_none());
    }

    #[test]
    fn cpu_is_the_busy_share_between_samples() {
        let a = CpuSample {
            total: 1000,
            idle: 800,
        };
        let b = CpuSample {
            total: 1200,
            idle: 850,
        };
        assert_eq!(cpu_percent(a, b), Some(75.0));
        assert_eq!(cpu_percent(a, a), None);
        assert_eq!(cpu_percent(b, a), None);
    }

    #[test]
    fn memory_is_total_less_available() {
        let m = mem_percent("MemTotal: 1000 kB\nMemFree: 10 kB\nMemAvailable: 390 kB\n");
        assert_eq!(m, Some(61.0));
        assert_eq!(mem_percent("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn host_json_has_every_field() {
        let _sandbox = crate::config::test_support::sandbox();
        let h = host_json();
        for k in ["cpu_percent", "mem_percent", "disk_percent"] {
            assert!(h.get(k).is_some(), "{k}");
        }
    }
}
