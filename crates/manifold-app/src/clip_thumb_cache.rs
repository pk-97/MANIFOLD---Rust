//! Sidecar disk cache for clip-thumbnail filmstrips (section 24 5c-2 P4).
//!
//! Survives reload so a rehearsed project's filmstrips are present on open
//! instead of re-captured. **Safety is by construction** — nothing here can stall
//! the content tick:
//!   * all disk IO runs on a dedicated worker thread (channel-fed),
//!   * the content thread only ever does an *async* atlas readback (the existing
//!     non-blocking [`crate::gpu_renderer`-style] pattern) and small bounded
//!     uploads,
//!   * load is **best-effort + validated** — a missing/short/old/wrong-geometry
//!     file is ignored and the clip simply re-captures, so a bad cache can never
//!     corrupt the live atlas (and even a bad cell self-heals on first play).
//!
//! Cells are stored RGBA8 (thumbnails are SDR previews), keyed by a per-clip
//! **content hash** — so the cache is project-independent and a clip carries its
//! thumbnails across projects, and an edit (new hash) simply misses and re-captures.

use ahash::{AHashMap, AHashSet};
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_renderer::gpu_readback::f16_to_f32;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 4] = b"MFS1";
const FORMAT_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const LOCK_FILE_NAME: &str = ".clip_thumbs.lock";

/// A per-clip content hash. Stable across reloads of the same clip content,
/// distinct for different content, changes on edit (→ cache miss → re-capture).
/// `layer` supplies the generator params (which live on the layer, not the clip),
/// so two clips on the same generator layer share a hash unless their per-clip
/// string params differ.
pub fn clip_content_hash(clip: &TimelineClip, layer: &Layer) -> u64 {
    let mut h = ahash::AHasher::default();
    if !clip.video_clip_id.is_empty() {
        // Video: identity + source window. (File mtime isn't readily available
        // content-side; the video id + in-point + duration capture the visible
        // window, which is what the filmstrip shows.)
        0u8.hash(&mut h);
        clip.video_clip_id.hash(&mut h);
        clip.in_point.as_f32().to_bits().hash(&mut h);
        clip.duration_beats.as_f32().to_bits().hash(&mut h);
    } else if let Some(gp) = layer.gen_params() {
        // Generator: type + authored params + per-clip string params.
        1u8.hash(&mut h);
        gp.generator_type().as_str().hash(&mut h);
        for v in gp.params.iter() {
            v.value.to_bits().hash(&mut h);
        }
        if let Some(sp) = &clip.string_params {
            for (k, v) in sp {
                k.hash(&mut h);
                v.hash(&mut h);
            }
        }
    } else {
        return 0; // not a thumbnailable clip
    }
    h.finish()
}

/// One clip's filmstrip: each captured cell index and its tightly-packed RGBA8
/// pixels (`cell_w * cell_h * 4` bytes).
pub type StripCells = Vec<(u32, Vec<u8>)>;

enum CacheMsg {
    /// A full clip-atlas persist readback, still packed as f16 (BUG-035): the
    /// content thread hands over the raw bytes untouched (`try_read_packed()`
    /// — a memcpy, no per-pixel work) and the worker does the f16→u8 convert
    /// + per-cell slice + disk write, all off the content thread.
    StoreAtlas {
        atlas_f16: Vec<u8>,
        atlas_w: u32,
        layout: Vec<(manifold_core::ClipId, u32, u32)>,
        hashes: AHashMap<String, u64>,
        cols: u32,
    },
    Load {
        hash: u64,
    },
    Shutdown,
}

/// A loaded strip handed back to the content thread for upload into the atlas.
pub struct LoadedStrip {
    pub hash: u64,
    pub cells: StripCells,
}

/// Worker-backed sidecar cache. Construct once; the content thread drives it with
/// `request_load`, `store_atlas`, and `drain_loaded`. Cell geometry is fixed at
/// construction and validated on every load.
pub struct ClipThumbCache {
    tx: Sender<CacheMsg>,
    rx_loaded: Receiver<LoadedStrip>,
    /// Hashes already requested this session, so a still-loading clip isn't
    /// re-requested every frame.
    requested: AHashSet<u64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ClipThumbCache {
    /// Create the cache + spawn its worker. Returns `None` if no cache directory
    /// is available (then thumbnails simply don't persist — no error).
    pub fn new(cell_w: u32, cell_h: u32) -> Option<Self> {
        let dir = cache_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let (tx, rx) = std::sync::mpsc::channel::<CacheMsg>();
        let (tx_loaded, rx_loaded) = std::sync::mpsc::channel::<LoadedStrip>();
        let worker_dir = dir.clone();
        let handle = std::thread::Builder::new()
            .name("clip-thumb-cache".into())
            .spawn(move || worker(worker_dir, cell_w, cell_h, rx, tx_loaded))
            .ok()?;
        Some(Self {
            tx,
            rx_loaded,
            requested: AHashSet::new(),
            handle: Some(handle),
        })
    }

    /// Request a background load of `hash`'s strip, once per session. Results are
    /// retrieved via [`Self::drain_loaded`].
    pub fn request_load(&mut self, hash: u64) {
        if self.requested.insert(hash) {
            let _ = self.tx.send(CacheMsg::Load { hash });
        }
    }

    /// Persist a clip-atlas persist readback. `atlas_f16` must be the tightly
    /// packed Rgba16Float bytes from `ReadbackRequest::try_read_packed()`
    /// (plain memcpy off the shared buffer — no conversion). Fire-and-forget:
    /// the f16→u8 convert, per-cell slice, and disk write all happen on the
    /// worker thread, so the caller pays only the channel send (BUG-035).
    pub fn store_atlas(
        &self,
        atlas_f16: Vec<u8>,
        atlas_w: u32,
        layout: Vec<(manifold_core::ClipId, u32, u32)>,
        hashes: AHashMap<String, u64>,
        cols: u32,
    ) {
        let _ = self.tx.send(CacheMsg::StoreAtlas {
            atlas_f16,
            atlas_w,
            layout,
            hashes,
            cols,
        });
    }

    /// Drain any strips the worker has finished loading.
    pub fn drain_loaded(&self) -> Vec<LoadedStrip> {
        self.rx_loaded.try_iter().collect()
    }
}

impl Drop for ClipThumbCache {
    fn drop(&mut self) {
        let _ = self.tx.send(CacheMsg::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Per-OS cache directory for the filmstrip store.
fn cache_dir() -> Option<PathBuf> {
    // macOS: ~/Library/Caches/manifold/clip_thumbs. Falls back to $HOME elsewhere.
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    #[cfg(target_os = "macos")]
    p.push("Library/Caches");
    #[cfg(not(target_os = "macos"))]
    p.push(".cache");
    p.push("manifold");
    p.push("clip_thumbs");
    Some(p)
}

fn strip_path(dir: &std::path::Path, hash: u64) -> PathBuf {
    dir.join(format!("{hash:016x}.strip"))
}

fn worker(
    dir: PathBuf,
    cell_w: u32,
    cell_h: u32,
    rx: Receiver<CacheMsg>,
    tx_loaded: Sender<LoadedStrip>,
) {
    worker_with_budget(&dir, cell_w, cell_h, rx, tx_loaded, MAX_CACHE_BYTES);
}

fn worker_with_budget(
    dir: &Path,
    cell_w: u32,
    cell_h: u32,
    rx: Receiver<CacheMsg>,
    tx_loaded: Sender<LoadedStrip>,
    budget: u64,
) {
    let cell_bytes = (cell_w * cell_h * 4) as usize;
    if let Err(err) = with_cache_lock(dir, || enforce_budget_locked(dir, budget)) {
        log::warn!("clip thumbnail cache startup cleanup failed for {dir:?}: {err}");
    }
    while let Ok(msg) = rx.recv() {
        match msg {
            CacheMsg::Shutdown => break,
            CacheMsg::StoreAtlas {
                atlas_f16,
                atlas_w,
                layout,
                hashes,
                cols,
            } => {
                let strips = slice_atlas_f16_for_store(
                    &atlas_f16, atlas_w, &layout, &hashes, cols, cell_w, cell_h,
                );
                if let Err(err) = with_cache_lock(dir, || {
                    persist_strips_locked(dir, budget, cell_w, cell_h, cell_bytes, &strips)
                }) {
                    log::warn!("clip thumbnail cache persist failed for {dir:?}: {err}");
                }
            }
            CacheMsg::Load { hash } => {
                match with_cache_lock(dir, || {
                    Ok(read_strip(dir, hash, cell_w, cell_h, cell_bytes))
                }) {
                    Ok(Some(cells)) => {
                        let _ = tx_loaded.send(LoadedStrip { hash, cells });
                    }
                    Ok(None) => {}
                    Err(err) => {
                        log::warn!("clip thumbnail cache load failed for {dir:?}: {err}");
                    }
                }
            }
        }
    }
}

struct ManagedStrip {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

fn with_cache_lock<T>(dir: &Path, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    let dir_metadata = std::fs::symlink_metadata(dir)?;
    if !dir_metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("refusing non-directory thumbnail cache root {dir:?}"),
        ));
    }
    let lock_path = dir.join(LOCK_FILE_NAME);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("refusing non-file cache lock path {lock_path:?}"),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.lock()?;
    let result = f();
    let unlock_result = lock_file.unlock();
    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(err),
    }
}

fn managed_hash(name: &OsStr) -> Option<u64> {
    let name = name.to_str()?;
    let hex = name.strip_suffix(".strip")?;
    if hex.len() != 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

fn scan_managed_strips(dir: &Path) -> std::io::Result<Vec<ManagedStrip>> {
    let mut strips = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(_hash) = managed_hash(&entry.file_name()) else {
            continue;
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                log::warn!(
                    "clip thumbnail cache metadata failed for {:?}: {err}",
                    entry.path()
                );
                return Err(err);
            }
        };
        if !file_type.is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                log::warn!(
                    "clip thumbnail cache metadata failed for {:?}: {err}",
                    entry.path()
                );
                return Err(err);
            }
        };
        let modified = metadata.modified().map_err(|err| {
            log::warn!(
                "clip thumbnail cache mtime failed for {:?}: {err}",
                entry.path()
            );
            err
        })?;
        strips.push(ManagedStrip {
            path: entry.path(),
            size: metadata.len(),
            modified,
        });
    }
    strips.sort_by(|a, b| {
        a.modified
            .cmp(&b.modified)
            .then_with(|| a.path.as_os_str().cmp(b.path.as_os_str()))
    });
    Ok(strips)
}

fn managed_regular_metadata(path: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    if path.file_name().and_then(managed_hash).is_none() {
        return Ok(None);
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if metadata.file_type().is_file() {
        Ok(Some(metadata))
    } else {
        Ok(None)
    }
}

fn total_size(strips: &[ManagedStrip]) -> std::io::Result<u64> {
    strips.iter().try_fold(0u64, |total, strip| {
        total.checked_add(strip.size).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "thumbnail cache size overflow",
            )
        })
    })
}

fn remove_oldest_until_fit(
    strips: &mut Vec<ManagedStrip>,
    total: &mut u64,
    target: &Path,
    new_size: u64,
    budget: u64,
) -> bool {
    let mut failed = AHashSet::new();
    loop {
        let Some(required) = total
            .checked_sub(
                strips
                    .iter()
                    .find(|strip| strip.path == target)
                    .map_or(0, |strip| strip.size),
            )
            .and_then(|base| base.checked_add(new_size))
        else {
            return false;
        };
        if required <= budget {
            return true;
        }
        let Some(index) = strips
            .iter()
            .position(|strip| strip.path != target && !failed.contains(&strip.path))
        else {
            return false;
        };
        let strip = &strips[index];
        let metadata = match managed_regular_metadata(&strip.path) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                failed.insert(strip.path.clone());
                continue;
            }
            Err(err) => {
                log::warn!(
                    "clip thumbnail cache eviction metadata failed for {:?}: {err}",
                    strip.path
                );
                failed.insert(strip.path.clone());
                continue;
            }
        };
        match std::fs::remove_file(&strip.path) {
            Ok(()) => {
                *total = total.saturating_sub(metadata.len());
                strips.remove(index);
            }
            Err(err) => {
                log::warn!(
                    "clip thumbnail cache eviction failed for {:?}: {err}",
                    strip.path
                );
                failed.insert(strip.path.clone());
            }
        }
    }
}

fn enforce_budget_locked(dir: &Path, budget: u64) -> std::io::Result<()> {
    let mut strips = scan_managed_strips(dir)?;
    let mut total = total_size(&strips)?;
    let target = Path::new("");
    if !remove_oldest_until_fit(&mut strips, &mut total, target, 0, budget) {
        log::warn!("clip thumbnail cache remains over budget in {dir:?}: {total} bytes");
    }
    Ok(())
}

fn persist_strips_locked(
    dir: &Path,
    budget: u64,
    cell_w: u32,
    cell_h: u32,
    cell_bytes: usize,
    strips_to_write: &[(u64, StripCells)],
) -> std::io::Result<()> {
    let mut managed = scan_managed_strips(dir)?;
    let mut total = total_size(&managed)?;
    for (hash, cells) in strips_to_write {
        if let Err(err) = persist_strip_locked(
            dir,
            budget,
            (cell_w, cell_h, cell_bytes),
            *hash,
            cells,
            &mut managed,
            &mut total,
        ) {
            log::warn!("clip thumbnail cache write failed for {hash:016x}: {err}");
        }
    }
    Ok(())
}

fn persist_strip_locked(
    dir: &Path,
    budget: u64,
    geometry: (u32, u32, usize),
    hash: u64,
    cells: &StripCells,
    managed: &mut Vec<ManagedStrip>,
    total: &mut u64,
) -> std::io::Result<()> {
    let (cell_w, cell_h, cell_bytes) = geometry;
    let valid: Vec<&(u32, Vec<u8>)> = cells
        .iter()
        .filter(|(_, b)| b.len() == cell_bytes)
        .collect();
    if valid.is_empty() {
        return Ok(());
    }
    let record_size = 4usize.checked_add(cell_bytes).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "thumbnail cell size overflow",
        )
    })?;
    let new_size = 20usize
        .checked_add(valid.len().checked_mul(record_size).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "thumbnail strip size overflow",
            )
        })?)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "thumbnail strip size overflow",
            )
        })?;
    let new_size = u64::try_from(new_size).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "thumbnail strip size overflow",
        )
    })?;
    if new_size > budget {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("thumbnail strip {hash:016x} exceeds cache budget"),
        ));
    }
    let path = strip_path(dir, hash);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("refusing to replace non-file thumbnail path {path:?}"),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    if !remove_oldest_until_fit(managed, total, &path, new_size, budget) {
        return Err(std::io::Error::other(
            "thumbnail cache cannot make room for strip",
        ));
    }
    let (tmp, mut file) = create_temp_file(dir, hash)?;
    let write_result = (|| {
        file.write_all(MAGIC)?;
        file.write_all(&FORMAT_VERSION.to_le_bytes())?;
        file.write_all(&cell_w.to_le_bytes())?;
        file.write_all(&cell_h.to_le_bytes())?;
        file.write_all(&(valid.len() as u32).to_le_bytes())?;
        for (idx, bytes) in &valid {
            file.write_all(&idx.to_le_bytes())?;
            file.write_all(bytes)?;
        }
        file.flush()
    })();
    drop(file);
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Some(index) = managed.iter().position(|strip| strip.path == path) {
        let old_size = managed[index].size;
        managed[index].size = new_size;
        managed[index].modified = SystemTime::now();
        *total = total.saturating_sub(old_size).saturating_add(new_size);
    } else {
        managed.push(ManagedStrip {
            path,
            size: new_size,
            modified: SystemTime::now(),
        });
        *total = total.saturating_add(new_size);
    }
    managed.sort_by(|a, b| {
        a.modified
            .cmp(&b.modified)
            .then_with(|| a.path.as_os_str().cmp(b.path.as_os_str()))
    });
    Ok(())
}

fn create_temp_file(dir: &Path, hash: u64) -> std::io::Result<(PathBuf, File)> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    create_temp_file_with_nonce(dir, hash, nonce)
}

fn create_temp_file_with_nonce(
    dir: &Path,
    hash: u64,
    nonce: u128,
) -> std::io::Result<(PathBuf, File)> {
    for attempt in 0..32u32 {
        let path = dir.join(format!(
            ".{hash:016x}.strip.tmp.{}.{}.{}",
            std::process::id(),
            nonce,
            attempt
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to allocate unique thumbnail temp file",
    ))
}

/// Atomic strip write (temp + rename), retained for focused format tests.
#[cfg(test)]
fn write_strip(
    dir: &Path,
    hash: u64,
    cell_w: u32,
    cell_h: u32,
    cells: &StripCells,
    cell_bytes: usize,
) -> std::io::Result<()> {
    with_cache_lock(dir, || {
        let mut managed = scan_managed_strips(dir)?;
        let mut total = total_size(&managed)?;
        persist_strip_locked(
            dir,
            u64::MAX,
            (cell_w, cell_h, cell_bytes),
            hash,
            cells,
            &mut managed,
            &mut total,
        )
    })
}

/// Validated strip read. Returns `None` on any mismatch (missing / short / wrong
/// magic / version / geometry) so the caller re-captures.
fn read_strip(
    dir: &Path,
    hash: u64,
    cell_w: u32,
    cell_h: u32,
    cell_bytes: usize,
) -> Option<StripCells> {
    let path = strip_path(dir, hash);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let mut f = File::open(&path).ok()?;
    let mut header = [0u8; 20];
    f.read_exact(&mut header).ok()?;
    if &header[0..4] != MAGIC {
        return None;
    }
    let ver = u32::from_le_bytes(header[4..8].try_into().ok()?);
    let fw = u32::from_le_bytes(header[8..12].try_into().ok()?);
    let fh = u32::from_le_bytes(header[12..16].try_into().ok()?);
    let count = u32::from_le_bytes(header[16..20].try_into().ok()?);
    if ver != FORMAT_VERSION || fw != cell_w || fh != cell_h || count > 4096 {
        return None;
    }
    let mut cells = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut idx_buf = [0u8; 4];
        f.read_exact(&mut idx_buf).ok()?;
        let idx = u32::from_le_bytes(idx_buf);
        let mut bytes = vec![0u8; cell_bytes];
        f.read_exact(&mut bytes).ok()?;
        cells.push((idx, bytes));
    }
    let _ = f.set_modified(SystemTime::now());
    Some(cells)
}

/// Slice each clip's cells out of a full **Rgba16Float** atlas persist
/// readback — tightly packed as `ReadbackRequest::try_read_packed()` returns
/// it (`width * 8` bytes/row: 4 channels × f16, no row padding) — converting
/// f16→u8 only for the pixels actually extracted into a cell, never the whole
/// atlas. `layout` is `(clip, cell idx, atlas cell)`; `hashes` maps clip id →
/// content hash; `cols` is the atlas column count. Runs on the
/// clip-thumb disk worker thread so this O(surface) conversion never touches
/// the content thread (BUG-035: the old path ran the full-atlas equivalent —
/// `ReadbackRequest::try_read()` — inline in the content tick, ~58ms/cycle on
/// the 8192×1152 clip atlas).
#[allow(clippy::too_many_arguments)]
pub fn slice_atlas_f16_for_store(
    atlas_f16: &[u8],
    atlas_w: u32,
    layout: &[(manifold_core::ClipId, u32, u32)],
    hashes: &AHashMap<String, u64>,
    cols: u32,
    cell_w: u32,
    cell_h: u32,
) -> Vec<(u64, StripCells)> {
    let cell_bytes = (cell_w * cell_h * 4) as usize;
    let atlas_stride = (atlas_w * 8) as usize; // f16: 4 channels × 2 bytes, tightly packed
    let mut by_hash: AHashMap<u64, StripCells> = AHashMap::new();
    for (clip, cell_idx, atlas_cell) in layout {
        let Some(&hash) = hashes.get(clip.as_str()) else {
            continue;
        };
        let gx = (atlas_cell % cols) * cell_w;
        let gy = (atlas_cell / cols) * cell_h;
        let mut bytes = vec![0u8; cell_bytes];
        let mut ok = true;
        'rows: for row in 0..cell_h {
            let src_row = ((gy + row) as usize) * atlas_stride + (gx as usize) * 8;
            let dst_row = (row as usize) * (cell_w as usize) * 4;
            for col in 0..cell_w as usize {
                let src_px = src_row + col * 8;
                if src_px + 8 > atlas_f16.len() {
                    ok = false;
                    break 'rows;
                }
                let dst_px = dst_row + col * 4;
                for ch in 0..4 {
                    let lo = atlas_f16[src_px + ch * 2];
                    let hi = atlas_f16[src_px + ch * 2 + 1];
                    let bits = u16::from_le_bytes([lo, hi]);
                    let f = f16_to_f32(bits);
                    bytes[dst_px + ch] = (f * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        if ok {
            by_hash.entry(hash).or_default().push((*cell_idx, bytes));
        }
    }
    by_hash.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_f16_extracts_and_converts_correct_cell_pixels() {
        // Same 2×1 grid of 2×2 cells as the RGBA8 test, but the atlas is
        // Rgba16Float, tightly packed (try_read_packed()'s layout: width * 8
        // bytes/row, no padding). Cell 1 (right half) is filled with an f16
        // marker (R = 1.0 → u8 255 after the (f*255).round() convert; A = 1.0).
        let (cols, cw, ch) = (2u32, 2u32, 2u32);
        let aw = cols * cw; // 4
        let ah = ch; // 2
        let one_f16: u16 = 0x3C00; // IEEE754 half for 1.0
        let mut atlas = vec![0u8; (aw * ah * 8) as usize];
        for y in 0..ah {
            for x in cw..aw {
                let px = ((y * aw + x) * 8) as usize;
                // R channel = 1.0, G/B = 0.0, A channel = 1.0.
                atlas[px..px + 2].copy_from_slice(&one_f16.to_le_bytes());
                atlas[px + 6..px + 8].copy_from_slice(&one_f16.to_le_bytes());
            }
        }
        let layout = vec![(manifold_core::ClipId::new("clipA"), 0u32, 1u32)];
        let mut hashes = AHashMap::new();
        hashes.insert("clipA".to_string(), 42u64);
        let out = slice_atlas_f16_for_store(&atlas, aw, &layout, &hashes, cols, cw, ch);
        assert_eq!(out.len(), 1);
        let (hash, cells) = &out[0];
        assert_eq!(*hash, 42);
        assert_eq!(cells.len(), 1);
        let (idx, bytes) = &cells[0];
        assert_eq!(*idx, 0);
        assert_eq!(bytes.len(), (cw * ch * 4) as usize);
        assert!(
            bytes
                .chunks(4)
                .all(|p| p[0] == 255 && p[1] == 0 && p[2] == 0 && p[3] == 255)
        );
    }

    #[test]
    fn roundtrip_write_then_read() {
        let dir = test_fixture_dir("roundtrip");
        let (cw, ch) = (2u32, 2u32);
        let cell_bytes = (cw * ch * 4) as usize;
        let cells: StripCells = vec![(0, vec![7u8; cell_bytes]), (3, vec![9u8; cell_bytes])];
        write_strip(&dir, 123, cw, ch, &cells, cell_bytes).unwrap();
        let read = read_strip(&dir, 123, cw, ch, cell_bytes).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0], (0, vec![7u8; cell_bytes]));
        assert_eq!(read[1], (3, vec![9u8; cell_bytes]));
        // Geometry mismatch → None.
        assert!(read_strip(&dir, 123, 4, 4, 64).is_none());
        // Missing → None.
        assert!(read_strip(&dir, 999, cw, ch, cell_bytes).is_none());
        cleanup_fixture_dir(&dir, &[strip_path(&dir, 123)]);
    }

    fn test_fixture_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mfst_test_{label}_{}_{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn cleanup_fixture_dir(dir: &Path, files: &[PathBuf]) {
        for file in files {
            let _ = std::fs::remove_file(file);
        }
        let _ = std::fs::remove_file(dir.join(LOCK_FILE_NAME));
        std::fs::remove_dir(dir).unwrap();
    }

    fn test_cells(byte: u8, cell_bytes: usize) -> StripCells {
        vec![(0, vec![byte; cell_bytes])]
    }

    #[test]
    fn startup_cleanup_evicts_oldest_managed_strips() {
        let dir = test_fixture_dir("startup");
        let cell_bytes = 4;
        write_strip(&dir, 1, 1, 1, &test_cells(1, cell_bytes), cell_bytes).unwrap();
        write_strip(&dir, 2, 1, 1, &test_cells(2, cell_bytes), cell_bytes).unwrap();
        let old = strip_path(&dir, 1);
        let new = strip_path(&dir, 2);
        File::open(&old).unwrap().set_modified(UNIX_EPOCH).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (loaded_tx, _loaded_rx) = std::sync::mpsc::channel();
        let worker_dir = dir.clone();
        let join =
            std::thread::spawn(move || worker_with_budget(&worker_dir, 1, 1, rx, loaded_tx, 28));
        tx.send(CacheMsg::Shutdown).unwrap();
        join.join().unwrap();
        assert!(!old.exists());
        assert!(new.exists());
        cleanup_fixture_dir(&dir, &[new]);
    }

    #[test]
    fn successful_load_refreshes_recency_for_eviction() {
        let dir = test_fixture_dir("recency");
        let cell_bytes = 4;
        write_strip(&dir, 1, 1, 1, &test_cells(1, cell_bytes), cell_bytes).unwrap();
        write_strip(&dir, 2, 1, 1, &test_cells(2, cell_bytes), cell_bytes).unwrap();
        let first = strip_path(&dir, 1);
        let second = strip_path(&dir, 2);
        File::open(&first)
            .unwrap()
            .set_modified(UNIX_EPOCH)
            .unwrap();
        File::open(&second)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        assert!(read_strip(&dir, 1, 1, 1, cell_bytes).is_some());
        with_cache_lock(&dir, || enforce_budget_locked(&dir, 28)).unwrap();
        assert!(first.exists());
        assert!(!second.exists());
        cleanup_fixture_dir(&dir, &[first]);
    }

    #[test]
    fn replacement_accounts_for_old_strip_size() {
        let dir = test_fixture_dir("replacement");
        let small = test_cells(1, 4);
        let large = vec![(0, vec![2; 8]), (1, vec![3; 8])];
        write_strip(&dir, 1, 1, 1, &small, 4).unwrap();
        let mut managed = scan_managed_strips(&dir).unwrap();
        let mut total = total_size(&managed).unwrap();
        persist_strip_locked(&dir, 44, (1, 1, 8), 1, &large, &mut managed, &mut total).unwrap();
        assert_eq!(std::fs::metadata(strip_path(&dir, 1)).unwrap().len(), 44);
        cleanup_fixture_dir(&dir, &[strip_path(&dir, 1)]);
    }

    #[test]
    fn oversized_strip_is_refused_without_eviction() {
        let dir = test_fixture_dir("oversized");
        let cells = test_cells(1, 4);
        let mut managed = Vec::new();
        let mut total = 0;
        let err = persist_strip_locked(&dir, 23, (1, 1, 4), 1, &cells, &mut managed, &mut total)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!strip_path(&dir, 1).exists());
        cleanup_fixture_dir(&dir, &[]);
    }

    #[test]
    fn unicode_unknown_filename_is_preserved() {
        let dir = test_fixture_dir("unicode");
        let unknown = dir.join("é.strip");
        std::fs::write(&unknown, b"keep").unwrap();
        with_cache_lock(&dir, || enforce_budget_locked(&dir, 0)).unwrap();
        assert!(unknown.exists());
        cleanup_fixture_dir(&dir, &[unknown]);
    }

    #[test]
    fn create_new_collision_does_not_remove_preexisting_temp() {
        let dir = test_fixture_dir("temp_collision");
        let hash = 0x1234_u64;
        let nonce = 42_u128;
        let preexisting = dir.join(format!(
            ".{hash:016x}.strip.tmp.{}.{}.0",
            std::process::id(),
            nonce
        ));
        std::fs::write(&preexisting, b"keep").unwrap();
        let (created, file) = create_temp_file_with_nonce(&dir, hash, nonce).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&preexisting).unwrap(), b"keep");
        std::fs::remove_file(&created).unwrap();
        cleanup_fixture_dir(&dir, &[preexisting]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cache_root_is_refused() {
        use std::os::unix::fs::symlink;
        let target = test_fixture_dir("root_target");
        let link = target.with_extension("link");
        symlink(&target, &link).unwrap();
        let err = with_cache_lock(&link, || Ok(())).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);
        assert!(!link.join(LOCK_FILE_NAME).exists());
        std::fs::remove_file(&link).unwrap();
        cleanup_fixture_dir(&target, &[]);
    }

    #[test]
    fn concurrent_workers_serialize_persist_and_keep_valid_strips() {
        let dir = test_fixture_dir("concurrent");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut joins = Vec::new();
        for hash in [1u64, 2u64] {
            let dir = dir.clone();
            let barrier = barrier.clone();
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                with_cache_lock(&dir, || {
                    let mut managed = scan_managed_strips(&dir)?;
                    let mut total = total_size(&managed)?;
                    persist_strip_locked(
                        &dir,
                        56,
                        (1, 1, 4),
                        hash,
                        &test_cells(hash as u8, 4),
                        &mut managed,
                        &mut total,
                    )
                })
                .unwrap();
            }));
        }
        for join in joins {
            join.join().unwrap();
        }
        assert!(read_strip(&dir, 1, 1, 1, 4).is_some());
        assert!(read_strip(&dir, 2, 1, 1, 4).is_some());
        cleanup_fixture_dir(&dir, &[strip_path(&dir, 1), strip_path(&dir, 2)]);
    }

    #[cfg(unix)]
    #[test]
    fn unknown_and_symlink_entries_are_preserved() {
        use std::os::unix::fs::symlink;
        let dir = test_fixture_dir("safety");
        let unknown = dir.join("keep.txt");
        std::fs::write(&unknown, b"keep").unwrap();
        let target = dir.join("target.bin");
        std::fs::write(&target, b"target").unwrap();
        let link = strip_path(&dir, 7);
        symlink(&target, &link).unwrap();
        with_cache_lock(&dir, || enforce_budget_locked(&dir, 0)).unwrap();
        assert!(unknown.exists());
        assert!(link.is_symlink());
        cleanup_fixture_dir(&dir, &[unknown, target, link]);
    }
}
