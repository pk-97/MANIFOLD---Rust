//! Persisted chain-segment fusion verdicts (the "measure once per machine"
//! cache the perf-gate doc anticipated, applied to the chain-segment layer).
//!
//! ## Why the segment layer
//!
//! A chain segment is identified by [`super::install::segment_key`] — a hash of
//! the member cards' *content* (topology + baked params), NOT their type. So a
//! verdict is bound to the exact graph it was measured on: change a card, change
//! the wiring, and the key changes, the lookup misses, and the worker
//! re-measures from scratch. A stale persisted entry can therefore never be
//! applied to a graph it wasn't measured against — which is exactly the property
//! that makes persisting it safe even though "graphs change all the time".
//!
//! ## What it saves
//!
//! The expensive part of compiling a segment is the gate measurement
//! ([`super::perf_gate::measure_segment_masked`]): hundreds of 4K frames per
//! candidate region mask, run on the background worker. Codegen (concat + fuse +
//! retarget) is cheap CPU work by comparison. So this cache persists only the
//! *verdict* — fuse-with-this-mask, or keep-per-card — and the worker still does
//! the (cheap, deterministic) codegen on a hit. On a miss it measures as before
//! and records the result.
//!
//! Net effect: the first launch on a given Mac measures each novel segment once;
//! every launch after (and every reopen of the same show project) skips the GPU
//! measurement and the post-project-load settling churn it caused.
//!
//! ## Invalidation
//!
//! The on-disk file carries the GPU [`device name`](manifold_gpu::GpuDevice::device_name)
//! and a [`FORMAT_VERSION`]. A verdict only loads when both match the running
//! process — a different GPU or a bumped codegen/partitioning version starts from
//! an empty cache (re-measures, then overwrites). Bump [`FORMAT_VERSION`] whenever
//! a change to region partitioning or fusion codegen could move a verdict.
//!
//! `MANIFOLD_FREEZE_CACHE=0` disables persistence entirely (always measure, never
//! read/write) — the escape hatch if a cache file is ever suspected bad.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use ahash::AHashMap;
use serde::{Deserialize, Serialize};

/// Bump when region partitioning or fusion codegen changes in a way that could
/// alter a recorded verdict (which mask wins, or fuse-vs-keep). A mismatch
/// discards the on-disk cache and re-measures.
const FORMAT_VERSION: u32 = 1;

/// A measured gate outcome for one segment.
#[derive(Clone, Debug, PartialEq)]
pub enum SegmentVerdict {
    /// Keep per-card — fusion did not clear the margin (or the candidate did not
    /// build / measure).
    KeepPerCard,
    /// Fuse, using this winning region mask (all-`true` = whole-segment fuse).
    Fuse(Vec<bool>),
}

/// Whether persistence is active this process. `MANIFOLD_FREEZE_CACHE=0`/`off`
/// turns it off (every segment measured fresh, nothing read or written). Read
/// once, cached.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("MANIFOLD_FREEZE_CACHE") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    })
}

/// `~/Library/Application Support/MANIFOLD/freeze-verdicts/segments.json` — same
/// base dir as `prefs.json` and the user preset packs. `None` if `$HOME` is
/// unset (then the cache simply no-ops, every segment measured fresh).
fn cache_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/MANIFOLD/freeze-verdicts")
            .join("segments.json"),
    )
}

// ── On-disk format ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct DiskVerdict {
    /// `true` = fuse, `false` = keep per-card.
    fuse: bool,
    /// Winning region mask, present only when `fuse`. Absent for keep-per-card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<Vec<bool>>,
}

#[derive(Serialize, Deserialize)]
struct DiskFile {
    version: u32,
    device: String,
    /// `segment_key` (as decimal string, since JSON object keys are strings) →
    /// verdict. `BTreeMap` for stable, diff-friendly on-disk ordering.
    segments: std::collections::BTreeMap<String, DiskVerdict>,
}

// ── In-memory store ──────────────────────────────────────────────────────────

struct Inner {
    /// The device this loaded map is valid for. A process only ever sees one
    /// GPU, but guarding on it keeps a copied-between-machines cache honest.
    device: String,
    map: AHashMap<u64, SegmentVerdict>,
}

static STORE: OnceLock<Mutex<Option<Inner>>> = OnceLock::new();

fn store() -> &'static Mutex<Option<Inner>> {
    STORE.get_or_init(|| Mutex::new(None))
}

/// Run `f` against the in-memory store for `device_name`, loading it from disk
/// on first use (or when the device changed — shouldn't happen in one process,
/// but it keeps the invariant local).
fn with_inner<R>(device_name: &str, f: impl FnOnce(&mut Inner) -> R) -> R {
    let mut guard = store().lock().expect("freeze verdict store poisoned");
    let needs_load = guard.as_ref().map(|i| i.device != device_name).unwrap_or(true);
    if needs_load {
        *guard = Some(load_from_disk(device_name));
    }
    f(guard.as_mut().expect("inner just set"))
}

/// Load + validate the on-disk cache for `device_name`; an empty store on any
/// failure (missing file, parse error, version/device mismatch).
fn load_from_disk(device_name: &str) -> Inner {
    let empty = || Inner {
        device: device_name.to_string(),
        map: AHashMap::default(),
    };
    let Some(path) = cache_path() else {
        return empty();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return empty();
    };
    let Ok(file) = serde_json::from_slice::<DiskFile>(&bytes) else {
        log::warn!("[freeze] segment verdict cache at {} is unreadable -> ignoring", path.display());
        return empty();
    };
    if file.version != FORMAT_VERSION || file.device != device_name {
        // Different codegen version or a cache from another GPU: re-measure.
        return empty();
    }
    let mut map = AHashMap::default();
    for (k, v) in file.segments {
        let Ok(key) = k.parse::<u64>() else { continue };
        let verdict = match (v.fuse, v.mask) {
            (true, Some(mask)) => SegmentVerdict::Fuse(mask),
            // A fuse verdict with no mask is malformed — skip it (re-measure).
            (true, None) => continue,
            (false, _) => SegmentVerdict::KeepPerCard,
        };
        map.insert(key, verdict);
    }
    log::info!(
        "[freeze] loaded {} segment verdict(s) from {}",
        map.len(),
        path.display(),
    );
    Inner {
        device: device_name.to_string(),
        map,
    }
}

/// Serialize `inner` and write it atomically (temp + rename) so a crash mid-write
/// can't leave a half-written file that fails to parse next launch.
fn write_to_disk(inner: &Inner) {
    let Some(path) = cache_path() else {
        return;
    };
    let segments = inner
        .map
        .iter()
        .map(|(k, v)| {
            let dv = match v {
                SegmentVerdict::Fuse(mask) => DiskVerdict {
                    fuse: true,
                    mask: Some(mask.clone()),
                },
                SegmentVerdict::KeepPerCard => DiskVerdict {
                    fuse: false,
                    mask: None,
                },
            };
            (k.to_string(), dv)
        })
        .collect();
    let file = DiskFile {
        version: FORMAT_VERSION,
        device: inner.device.clone(),
        segments,
    };
    let Ok(json) = serde_json::to_vec_pretty(&file) else {
        return;
    };
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        log::warn!("[freeze] could not create verdict cache dir {}: {e}", dir.display());
        return;
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &json).is_err() {
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log::warn!("[freeze] could not commit verdict cache {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

// ── Public API (called only from the chain-fusion worker thread) ─────────────

/// The persisted verdict for `key` on `device_name`, or `None` if not cached
/// (or persistence is disabled). On a hit the caller skips the GPU gate
/// measurement and rebuilds the segment from the recorded mask.
pub(crate) fn lookup(device_name: &str, key: u64) -> Option<SegmentVerdict> {
    if !enabled() {
        return None;
    }
    with_inner(device_name, |inner| inner.map.get(&key).cloned())
}

/// Record a freshly measured `verdict` for `key` and flush to disk. A no-op when
/// persistence is disabled.
pub(crate) fn record(device_name: &str, key: u64, verdict: SegmentVerdict) {
    if !enabled() {
        return;
    }
    with_inner(device_name, |inner| {
        inner.map.insert(key, verdict);
        write_to_disk(inner);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_roundtrip_preserves_verdicts() {
        let inner = Inner {
            device: "TestGPU".to_string(),
            map: {
                let mut m = AHashMap::default();
                m.insert(1u64, SegmentVerdict::KeepPerCard);
                m.insert(2u64, SegmentVerdict::Fuse(vec![true, false, true]));
                m
            },
        };
        // Round-trip through the on-disk representation directly (no real $HOME
        // dependency): build a DiskFile, serialize, parse, and reload the map.
        let segments = inner
            .map
            .iter()
            .map(|(k, v)| {
                let dv = match v {
                    SegmentVerdict::Fuse(mask) => DiskVerdict { fuse: true, mask: Some(mask.clone()) },
                    SegmentVerdict::KeepPerCard => DiskVerdict { fuse: false, mask: None },
                };
                (k.to_string(), dv)
            })
            .collect();
        let file = DiskFile { version: FORMAT_VERSION, device: inner.device.clone(), segments };
        let json = serde_json::to_vec_pretty(&file).expect("serialize");
        let back: DiskFile = serde_json::from_slice(&json).expect("parse");

        assert_eq!(back.version, FORMAT_VERSION);
        assert_eq!(back.device, "TestGPU");
        let keep = back.segments.get("1").expect("key 1");
        assert!(!keep.fuse);
        assert!(keep.mask.is_none());
        let fuse = back.segments.get("2").expect("key 2");
        assert!(fuse.fuse);
        assert_eq!(fuse.mask.as_deref(), Some(&[true, false, true][..]));
    }

    #[test]
    fn version_mismatch_is_dropped_on_load() {
        let file = DiskFile {
            version: FORMAT_VERSION + 1,
            device: "TestGPU".to_string(),
            segments: std::collections::BTreeMap::new(),
        };
        let json = serde_json::to_vec(&file).expect("serialize");
        // Simulate the load-time guard.
        let parsed: DiskFile = serde_json::from_slice(&json).expect("parse");
        let valid = parsed.version == FORMAT_VERSION && parsed.device == "TestGPU";
        assert!(!valid, "a future format version must be rejected");
    }
}
