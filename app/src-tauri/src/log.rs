// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Minimal logging: stderr plus `<data_dir>/shredlab.log` (the release exe has
//! no console).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

/// Opens (appends to) the log file. Safe to call more than once.
pub fn init(data_dir: &Path) {
    START.get_or_init(Instant::now);
    let file = std::fs::create_dir_all(data_dir).ok().and_then(|_| {
        let path = data_dir.join("shredlab.log");
        // Keep the log from growing forever.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::rename(&path, data_dir.join("shredlab.old.log"));
        }
        OpenOptions::new().create(true).append(true).open(path).ok()
    });
    let slot = FILE.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = slot.lock() {
        *g = file;
    }
}

pub fn write(msg: &str) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    let line = format!("[{t:8.2}s] {msg}");
    eprintln!("{line}");
    if let Some(slot) = FILE.get() {
        if let Ok(mut g) = slot.lock() {
            if let Some(f) = g.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
