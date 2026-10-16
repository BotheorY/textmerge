// SPDX-License-Identifier: MIT
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Prefer a log beside the executable (USB friendly), then a per-user fallback.
pub fn error(message: &str) -> Option<PathBuf> {
    let _guard = LOG_LOCK.lock().ok()?;
    let executable_directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let fallback = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("TextMerge")
        .join("logs");
    for directory in [executable_directory.join("logs"), fallback] {
        if fs::create_dir_all(&directory).is_err() {
            continue;
        }
        let path = directory.join("TextMerge-errors.log");
        // Bounded rotation prevents repeated errors from filling a removable drive.
        if fs::metadata(&path).is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
            let _ = fs::rename(&path, directory.join("TextMerge-errors.previous.log"));
        }
        if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(&path) {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if writeln!(log, "[{seconds} UTC unix seconds] {message}").is_ok() {
                return Some(path);
            }
        }
    }
    None
}
