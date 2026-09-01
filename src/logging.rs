//! JSONL logging of snapshots to %LOCALAPPDATA%/syswatch/logs/YYYY-MM-DD.jsonl.
//!
//! Hot path: `append()` is called once per tick (default 1 Hz). Allocating a
//! new String and re-opening the file every tick on Windows used to cost
//! 5-6% CPU on its own. We now stream the JSON directly into a cached
//! `BufWriter` and only rotate to a new day-file when the date actually
//! changes.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::dirs_home;
use crate::stats::Snapshot;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

#[cfg(windows)]
const FILE_SHARE: u32 = 0x00000007; // FILE_SHARE_READ | WRITE | DELETE

/// Resolve the log directory once. Order of preference:
/// 1. SYSWATCH_LOG_DIR env var (explicit override, also set by service.rs)
/// 2. %PROGRAMDATA%/syswatch/logs when running as a Windows service
///    (LocalSystem has no LOCALAPPDATA pointing at the user profile)
/// 3. %LOCALAPPDATA%/syswatch/logs for interactive runs
pub(crate) fn log_dir() -> Option<PathBuf> {
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

/// All candidate log directories (service + interactive), deduplicated.
/// Used by the history view so it sees data from both the service
/// (%PROGRAMDATA%) and interactive --log runs (%LOCALAPPDATA%).
pub(crate) fn log_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("SYSWATCH_LOG_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(pd) = std::env::var("PROGRAMDATA") {
        dirs.push(PathBuf::from(pd).join("syswatch").join("logs"));
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(la).join("syswatch").join("logs"));
    } else if let Some(h) = dirs_home() {
        dirs.push(h.join("AppData").join("Local").join("syswatch").join("logs"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
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
///
/// Caches the open `BufWriter` keyed by today's date so we only
/// `open()` once per day and amortise the per-tick syscall cost.
///
/// Every line is **flushed** before we return. The total volume is
/// ~1.2 KB/s, so the extra `flush()` is irrelevant compared to the
/// risk of losing the most recent sample on a crash or restart.
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

    let mut guard = LOG_FILE.lock().unwrap();
    if guard.path.as_deref() != Some(path.as_path()) {
        // New day or first write — drop the old handle and open a new one.
        guard.writer.take();
        match open_writer(&path) {
            Ok(w) => guard.writer = Some(w),
            Err(_) => {
                guard.path = None;
                    return;
            }
        }
        guard.path = Some(path);
    }
    if let Some(writer) = guard.writer.as_mut() {
        if serde_json::to_writer(&mut *writer, snap).is_err() {
            // Drop the writer so we don't keep retrying into a poisoned sink.
            guard.writer.take();
            return;
        }
        let _ = writer.write_all(b"\n");
        // Flush every tick so a crash / kill never loses the last sample.
        let _ = writer.flush();
    }
}

/// Cached `BufWriter` for today's log file.
struct CachedWriter {
    writer: Option<BufWriter<fs::File>>,
    path: Option<PathBuf>,
}

static LOG_FILE: Mutex<CachedWriter> = Mutex::new(CachedWriter {
    writer: None,
    path: None,
});

fn open_writer(path: &Path) -> std::io::Result<BufWriter<fs::File>> {
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(windows)]
    {
        // Allow concurrent readers (the dashboard, file watchers) without
        // forcing our writer to wait for them to close first. Keeps the
        // open cheap and non-blocking on Windows.
        opts.share_mode(FILE_SHARE);
    }
    let file = opts.open(path)?;
    // 8 KB buffer is enough for one ~1.2 KB JSONL line plus the newline;
    // bigger buffers only help when writes are larger than a line, which
    // they are not here.
    Ok(BufWriter::with_capacity(8 * 1024, file))
}
