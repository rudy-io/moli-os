use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Stats {
    pub uptime_ms: u64,
    pub devices: usize,
    pub points: usize,
    pub drivers: usize,
    pub drivers_running: usize,
    /// Resident memory of this process (Linux only).
    pub rss_bytes: Option<u64>,
}

/// Reads `VmRSS` from `/proc/self/status`.
pub(crate) fn rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}
