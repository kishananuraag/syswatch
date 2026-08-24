//! JSONL logging of snapshots to %LOCALAPPDATA%/syswatch/logs/YYYY-MM-DD.jsonl.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::config::dirs_home;
use crate::stats::Snapshot;

/// Resolve the log directory once. Order of preference:
/// 1. SYSWATCH_LOG_DIR env var (explicit override, also set by service.rs)
/// 2. %PROGRAMDATA%/syswatch/logs when running as a Windows service
///    (LocalSystem has no LOCALAPPDATA pointing at the user profile)
/// 3. %LOCALAPPDATA%/syswatch/logs for interactive runs
fn log_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SYSWATCH_LOG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if std::env::var("SYSWATCH_SERVICE").is_ok() {
        return std::env::var("PROGRAMDATA")
            .ok()
            .map(PathBuf::from)
            .map(|b| b.join("syswatch").join("logs"));
    }
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join("AppData").join("Local")))?;
    Some(base.join("syswatch").join("logs"))
}

/// Delete log files older than `retention_days` (by modification time).
pub fn prune(retention_days: u64) {
    let Some(dir) = log_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let max_age = std::time::Duration::from_secs(retention_days * 86_400);
    for entry in entries.flatten() {
        let expired = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age > max_age);
        if expired == Some(true) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Append one compact JSON line for this snapshot to today's log file.
pub fn append(snap: &Snapshot) {
    let Some(dir) = log_dir() else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    // Timestamp is RFC 3339 UTC; first 10 chars are the YYYY-MM-DD date.
    let date = snap.timestamp.get(..10).unwrap_or("unknown");
    let path = dir.join(format!("{}.jsonl", date));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        if let Ok(line) = serde_json::to_string(snap) {
            let _ = writeln!(f, "{}", line);
        }
    }
}
