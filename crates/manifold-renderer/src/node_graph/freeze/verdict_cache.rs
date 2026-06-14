//! Persisted fusion verdicts (the "measure once per machine" cache the perf-gate
//! doc anticipated). Two caches share this machinery:
//!
//! - [`Kind::Card`] — per-card canonical-preset verdicts produced by
//!   [`super::perf_gate::tune_all`] at startup.
//! - [`Kind::Segment`] — cross-card chain-segment verdicts produced by the
//!   chain-fusion worker ([`super::install::compile_segment_view`]).
//!
//! ## Why this is safe to persist
//!
//! Every entry is keyed by a **content hash** of the graph it was measured on —
//! [`super::install::def_content_key`] for a card, [`super::install::segment_key`]
//! for a segment. So a verdict is bound to the exact graph that produced it:
//! edit a card, rewire a chain, or ship a different preset, and the key changes,
//! the lookup misses, and the gate re-measures. A stale entry can never be
//! applied to a graph it wasn't measured on — which is the property that makes
//! persisting safe even though graphs change.
//!
//! ## What it saves
//!
//! The expensive part of a verdict is the GPU gate measurement — hundreds of 4K
//! frames per candidate region mask. Codegen (fuse + retarget) is cheap CPU work
//! by comparison. So this persists only the *verdict* (the winning region mask,
//! or don't-fuse); on a hit the caller skips the measurement and rebuilds from
//! the recorded mask. First launch on a Mac measures each novel graph once;
//! every launch after loads the verdicts and skips the GPU work.
//!
//! ## Invalidation
//!
//! Each file carries the GPU [device name](manifold_gpu::GpuDevice::device_name)
//! and a [`FORMAT_VERSION`]. A file only loads when both match the running
//! process; a different GPU or a bumped codegen version starts empty and
//! re-measures. Bump [`FORMAT_VERSION`] whenever region partitioning or fusion
//! codegen could move a verdict. `MANIFOLD_FREEZE_CACHE=0` disables persistence
//! entirely (always measure, never read or write).

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use ahash::AHashMap;
use serde::{Deserialize, Serialize};

/// Bump when region partitioning or fusion codegen changes in a way that could
/// alter a recorded verdict (which mask wins, or fuse-vs-keep). A mismatch
/// discards the on-disk cache and re-measures.
const FORMAT_VERSION: u32 = 1;

/// Which verdict cache — selects the on-disk filename and a separate in-memory
/// store, so card and segment verdicts never clobber each other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Per-card canonical-preset verdicts (`tune_all`).
    Card,
    /// Cross-card chain-segment verdicts (chain-fusion worker).
    Segment,
}

impl Kind {
    fn filename(self) -> &'static str {
        match self {
            Kind::Card => "cards.json",
            Kind::Segment => "segments.json",
        }
    }
}

/// A measured gate outcome.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Verdict {
    /// Don't fuse — fusion did not clear the margin (or the candidate did not
    /// build / measure). For a segment this means "render per-card"; for a card,
    /// "render unfused".
    DontFuse,
    /// Fuse, using this winning region mask (all-`true` = whole-graph fuse).
    Fuse(Vec<bool>),
}

/// Whether persistence is active this process. `MANIFOLD_FREEZE_CACHE=0`/`off`
/// turns it off (everything measured fresh, nothing read or written). Read once.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("MANIFOLD_FREEZE_CACHE") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    })
}

/// `~/Library/Application Support/MANIFOLD/freeze-verdicts/<file>` — same base
/// dir as `prefs.json` and the user preset packs. `None` if `$HOME` is unset
/// (then the cache no-ops, everything measured fresh).
fn cache_path(kind: Kind) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/MANIFOLD/freeze-verdicts")
            .join(kind.filename()),
    )
}

// ── On-disk format ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct DiskVerdict {
    /// `true` = fuse, `false` = don't fuse.
    fuse: bool,
    /// Winning region mask, present only when `fuse`. Absent for don't-fuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<Vec<bool>>,
}

#[derive(Serialize, Deserialize)]
struct DiskFile {
    version: u32,
    device: String,
    /// content key (decimal string, since JSON object keys are strings) →
    /// verdict. `BTreeMap` for stable, diff-friendly on-disk ordering.
    entries: std::collections::BTreeMap<String, DiskVerdict>,
}

impl DiskVerdict {
    fn from_verdict(v: &Verdict) -> Self {
        match v {
            Verdict::Fuse(mask) => DiskVerdict { fuse: true, mask: Some(mask.clone()) },
            Verdict::DontFuse => DiskVerdict { fuse: false, mask: None },
        }
    }
    /// `None` for a malformed entry (a `fuse: true` with no mask) — skip it,
    /// re-measure.
    fn to_verdict(&self) -> Option<Verdict> {
        match (self.fuse, &self.mask) {
            (true, Some(mask)) => Some(Verdict::Fuse(mask.clone())),
            (true, None) => None,
            (false, _) => Some(Verdict::DontFuse),
        }
    }
}

// ── In-memory store (one per Kind) ───────────────────────────────────────────

struct Inner {
    /// The device this loaded map is valid for. A process only sees one GPU, but
    /// guarding on it keeps a copied-between-machines cache honest.
    device: String,
    map: AHashMap<u64, Verdict>,
}

fn store(kind: Kind) -> &'static Mutex<Option<Inner>> {
    static CARD: OnceLock<Mutex<Option<Inner>>> = OnceLock::new();
    static SEGMENT: OnceLock<Mutex<Option<Inner>>> = OnceLock::new();
    match kind {
        Kind::Card => CARD.get_or_init(|| Mutex::new(None)),
        Kind::Segment => SEGMENT.get_or_init(|| Mutex::new(None)),
    }
}

/// Run `f` against the in-memory store for `(kind, device_name)`, loading from
/// disk on first use (or when the device changed — shouldn't happen in one
/// process, but it keeps the invariant local).
fn with_inner<R>(kind: Kind, device_name: &str, f: impl FnOnce(&mut Inner) -> R) -> R {
    let mut guard = store(kind).lock().expect("freeze verdict store poisoned");
    let needs_load = guard.as_ref().map(|i| i.device != device_name).unwrap_or(true);
    if needs_load {
        *guard = Some(load_from_disk(kind, device_name));
    }
    f(guard.as_mut().expect("inner just set"))
}

/// Load + validate the on-disk cache; an empty store on any failure (missing
/// file, parse error, version/device mismatch).
fn load_from_disk(kind: Kind, device_name: &str) -> Inner {
    let empty = || Inner { device: device_name.to_string(), map: AHashMap::default() };
    let Some(path) = cache_path(kind) else {
        return empty();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return empty();
    };
    let Ok(file) = serde_json::from_slice::<DiskFile>(&bytes) else {
        log::warn!("[freeze] verdict cache at {} is unreadable -> ignoring", path.display());
        return empty();
    };
    if file.version != FORMAT_VERSION || file.device != device_name {
        // Different codegen version or a cache from another GPU: re-measure.
        return empty();
    }
    let mut map = AHashMap::default();
    for (k, v) in &file.entries {
        if let (Ok(key), Some(verdict)) = (k.parse::<u64>(), v.to_verdict()) {
            map.insert(key, verdict);
        }
    }
    log::info!("[freeze] loaded {} verdict(s) from {}", map.len(), path.display());
    Inner { device: device_name.to_string(), map }
}

/// Serialize `inner` and write it atomically (temp + rename) so a crash
/// mid-write can't leave a half-written file that fails to parse next launch.
fn write_to_disk(kind: Kind, inner: &Inner) {
    let Some(path) = cache_path(kind) else {
        return;
    };
    let entries = inner
        .map
        .iter()
        .map(|(k, v)| (k.to_string(), DiskVerdict::from_verdict(v)))
        .collect();
    let file = DiskFile { version: FORMAT_VERSION, device: inner.device.clone(), entries };
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

// ── Public API ───────────────────────────────────────────────────────────────

/// The persisted verdict for `key`, or `None` if not cached (or persistence is
/// disabled). On a hit the caller skips the GPU gate measurement and rebuilds
/// from the recorded mask.
pub(crate) fn lookup(kind: Kind, device_name: &str, key: u64) -> Option<Verdict> {
    if !enabled() {
        return None;
    }
    with_inner(kind, device_name, |inner| inner.map.get(&key).cloned())
}

/// Record a single freshly measured `verdict` for `key` and flush to disk —
/// incremental, used by the chain-fusion worker as segments arrive one at a
/// time. A no-op when persistence is disabled.
pub(crate) fn record(kind: Kind, device_name: &str, key: u64, verdict: Verdict) {
    if !enabled() {
        return;
    }
    with_inner(kind, device_name, |inner| {
        inner.map.insert(key, verdict);
        write_to_disk(kind, inner);
    });
}

/// Replace the entire cache with `map` and flush once. Used by `tune_all`, which
/// holds the complete fresh verdict set for every current preset: writing the
/// whole thing in one go both persists the run and prunes orphaned keys left by
/// preset versions that no longer exist. A no-op when persistence is disabled.
pub(crate) fn replace_all(kind: Kind, device_name: &str, map: AHashMap<u64, Verdict>) {
    if !enabled() {
        return;
    }
    let inner = Inner { device: device_name.to_string(), map };
    write_to_disk(kind, &inner);
    *store(kind).lock().expect("freeze verdict store poisoned") = Some(inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_roundtrip_preserves_verdicts() {
        let mut map: AHashMap<u64, Verdict> = AHashMap::default();
        map.insert(1u64, Verdict::DontFuse);
        map.insert(2u64, Verdict::Fuse(vec![true, false, true]));
        let inner = Inner { device: "TestGPU".to_string(), map };

        let entries = inner
            .map
            .iter()
            .map(|(k, v)| (k.to_string(), DiskVerdict::from_verdict(v)))
            .collect();
        let file = DiskFile { version: FORMAT_VERSION, device: inner.device.clone(), entries };
        let json = serde_json::to_vec_pretty(&file).expect("serialize");
        let back: DiskFile = serde_json::from_slice(&json).expect("parse");

        assert_eq!(back.version, FORMAT_VERSION);
        assert_eq!(back.device, "TestGPU");
        assert_eq!(back.entries.get("1").unwrap().to_verdict(), Some(Verdict::DontFuse));
        assert_eq!(
            back.entries.get("2").unwrap().to_verdict(),
            Some(Verdict::Fuse(vec![true, false, true]))
        );
    }

    #[test]
    fn fuse_without_mask_is_rejected() {
        // A malformed entry (fuse:true, no mask) must not load as a fuse verdict.
        let dv = DiskVerdict { fuse: true, mask: None };
        assert_eq!(dv.to_verdict(), None);
    }

    #[test]
    fn version_mismatch_is_dropped_on_load() {
        let file = DiskFile {
            version: FORMAT_VERSION + 1,
            device: "TestGPU".to_string(),
            entries: std::collections::BTreeMap::new(),
        };
        let json = serde_json::to_vec(&file).expect("serialize");
        let parsed: DiskFile = serde_json::from_slice(&json).expect("parse");
        let valid = parsed.version == FORMAT_VERSION && parsed.device == "TestGPU";
        assert!(!valid, "a future format version must be rejected");
    }

    #[test]
    fn distinct_kinds_use_distinct_files() {
        assert_ne!(Kind::Card.filename(), Kind::Segment.filename());
    }
}
