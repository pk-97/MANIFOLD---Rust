//! Live config reload.
//!
//! Polls `~/Library/Application Support/TVLEDMirror/flags.conf` for `mtime`
//! changes (1s cadence). On change, re-parses through the same clap `Cli`
//! used at startup and atomically swaps the live-tunable subset
//! ([`DynamicConfig`]) into [`SharedState`]. Capture-time settings (display,
//! ip/port, strips/leds, hdr, p3) are not hot-swappable — those still
//! require a process restart, and we just log if the user changed them.
//!
//! Polling beats a notify-style file watcher for this — flags.conf is one
//! file, lives in a stable path, and a 1s tick is plenty for "edit-save-see"
//! tuning loops without burning CPU.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clap::Parser;

use crate::{Cli, DynamicConfig, SharedState};

/// Flags-file path. Mirrors what [`launch.sh`] reads, so editing the same
/// file affects both Finder/Dock launches and live-running processes.
fn flags_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/TVLEDMirror/flags.conf");
    Some(p)
}

pub fn spawn(state: Arc<SharedState>) {
    let Some(path) = flags_path() else {
        return;
    };
    std::thread::spawn(move || run(path, state));
}

fn run(path: PathBuf, state: Arc<SharedState>) {
    // Seed with the current mtime so we don't double-apply on first tick.
    let mut last_mtime: Option<SystemTime> = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

    log::info!(
        "live config reload watching {} (1s cadence)",
        path.display()
    );

    loop {
        std::thread::sleep(Duration::from_secs(1));

        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if Some(mtime) == last_mtime {
            continue;
        }
        last_mtime = Some(mtime);

        match parse_and_apply(&path, &state) {
            Ok(changes) => log::info!("config reloaded: {} changes applied", changes),
            Err(e) => log::warn!("config reload failed: {e}"),
        }
    }
}

fn parse_and_apply(path: &PathBuf, state: &Arc<SharedState>) -> Result<usize, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    // Same word-splitting launch.sh does: strip comments, ignore blanks.
    let mut argv: Vec<String> = vec!["tv-led-mirror".to_string()];
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        argv.extend(line.split_whitespace().map(String::from));
    }

    let cli = Cli::try_parse_from(argv).map_err(|e| format!("parse: {e}"))?;
    let new_dynamic = DynamicConfig::from_cli(&cli);

    // Atomic swap. Quick lock; we only hold for the assignment.
    let mut guard = state.config.write();
    let _old = std::mem::replace(&mut *guard, new_dynamic);
    Ok(1)
}
