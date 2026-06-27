//! Sidecar disk cache for clip-thumbnail filmstrips (§24 5c-2 P4).
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
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

const MAGIC: &[u8; 4] = b"MFS1";
const FORMAT_VERSION: u32 = 1;

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
        for v in &gp.param_values {
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
    Store { hash: u64, cells: StripCells },
    Load { hash: u64 },
    Shutdown,
}

/// A loaded strip handed back to the content thread for upload into the atlas.
pub struct LoadedStrip {
    pub hash: u64,
    pub cells: StripCells,
}

/// Worker-backed sidecar cache. Construct once; the content thread drives it with
/// `request_load`, `store`, and `drain_loaded`. Cell geometry is fixed at
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

    /// Persist a clip's captured cells (RGBA8). Fire-and-forget to the worker.
    pub fn store(&self, hash: u64, cells: StripCells) {
        if !cells.is_empty() {
            let _ = self.tx.send(CacheMsg::Store { hash, cells });
        }
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
    let cell_bytes = (cell_w * cell_h * 4) as usize;
    while let Ok(msg) = rx.recv() {
        match msg {
            CacheMsg::Shutdown => break,
            CacheMsg::Store { hash, cells } => {
                let _ = write_strip(&dir, hash, cell_w, cell_h, &cells, cell_bytes);
            }
            CacheMsg::Load { hash } => {
                if let Some(cells) = read_strip(&dir, hash, cell_w, cell_h, cell_bytes) {
                    let _ = tx_loaded.send(LoadedStrip { hash, cells });
                }
            }
        }
    }
}

/// Atomic strip write (temp + rename). Best-effort; errors are swallowed.
fn write_strip(
    dir: &std::path::Path,
    hash: u64,
    cell_w: u32,
    cell_h: u32,
    cells: &StripCells,
    cell_bytes: usize,
) -> std::io::Result<()> {
    let valid: Vec<&(u32, Vec<u8>)> = cells.iter().filter(|(_, b)| b.len() == cell_bytes).collect();
    if valid.is_empty() {
        return Ok(());
    }
    let tmp = dir.join(format!("{hash:016x}.strip.tmp"));
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(MAGIC)?;
        f.write_all(&FORMAT_VERSION.to_le_bytes())?;
        f.write_all(&cell_w.to_le_bytes())?;
        f.write_all(&cell_h.to_le_bytes())?;
        f.write_all(&(valid.len() as u32).to_le_bytes())?;
        for (idx, bytes) in &valid {
            f.write_all(&idx.to_le_bytes())?;
            f.write_all(bytes)?;
        }
        f.flush()?;
    }
    std::fs::rename(&tmp, strip_path(dir, hash))
}

/// Validated strip read. Returns `None` on any mismatch (missing / short / wrong
/// magic / version / geometry) so the caller re-captures.
fn read_strip(
    dir: &std::path::Path,
    hash: u64,
    cell_w: u32,
    cell_h: u32,
    cell_bytes: usize,
) -> Option<StripCells> {
    let mut f = std::fs::File::open(strip_path(dir, hash)).ok()?;
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
    Some(cells)
}

/// Slice each clip's cells out of a full RGBA8 atlas readback, grouped by content
/// hash, ready to [`ClipThumbCache::store`]. `layout` is `(clip, cell idx, atlas
/// cell)`; `hashes` maps clip id → content hash; `cols` is the atlas column count.
#[allow(clippy::too_many_arguments)]
pub fn slice_atlas_for_store(
    atlas_rgba8: &[u8],
    atlas_w: u32,
    layout: &[(manifold_core::ClipId, u32, u32)],
    hashes: &AHashMap<String, u64>,
    cols: u32,
    cell_w: u32,
    cell_h: u32,
) -> Vec<(u64, StripCells)> {
    let cell_bytes = (cell_w * cell_h * 4) as usize;
    let atlas_stride = (atlas_w * 4) as usize;
    let mut by_hash: AHashMap<u64, StripCells> = AHashMap::new();
    for (clip, cell_idx, atlas_cell) in layout {
        let Some(&hash) = hashes.get(clip.as_str()) else {
            continue;
        };
        let gx = (atlas_cell % cols) * cell_w;
        let gy = (atlas_cell / cols) * cell_h;
        // Extract the cell's rows from the atlas buffer.
        let mut bytes = vec![0u8; cell_bytes];
        let row_len = (cell_w * 4) as usize;
        let mut ok = true;
        for row in 0..cell_h {
            let src = ((gy + row) as usize) * atlas_stride + (gx as usize) * 4;
            let dst = (row as usize) * row_len;
            if src + row_len > atlas_rgba8.len() {
                ok = false;
                break;
            }
            bytes[dst..dst + row_len].copy_from_slice(&atlas_rgba8[src..src + row_len]);
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
    fn slice_extracts_correct_cell_pixels() {
        // 2×1 grid of 2×2 cells → atlas 4×2 RGBA8. Cell 0 = left, cell 1 = right.
        let (cols, cw, ch) = (2u32, 2u32, 2u32);
        let aw = cols * cw; // 4
        let ah = ch; // 2
        let mut atlas = vec![0u8; (aw * ah * 4) as usize];
        // Fill cell 1 (right half) with a marker (R=200).
        for y in 0..ah {
            for x in cw..aw {
                let i = ((y * aw + x) * 4) as usize;
                atlas[i] = 200;
                atlas[i + 3] = 255;
            }
        }
        let layout = vec![(manifold_core::ClipId::new("clipA"), 0u32, 1u32)]; // clipA bar0 → atlas cell 1
        let mut hashes = AHashMap::new();
        hashes.insert("clipA".to_string(), 42u64);
        let out = slice_atlas_for_store(&atlas, aw, &layout, &hashes, cols, cw, ch);
        assert_eq!(out.len(), 1);
        let (hash, cells) = &out[0];
        assert_eq!(*hash, 42);
        assert_eq!(cells.len(), 1);
        let (idx, bytes) = &cells[0];
        assert_eq!(*idx, 0);
        assert_eq!(bytes.len(), (cw * ch * 4) as usize);
        // Every pixel of the extracted cell should be the R=200 marker.
        assert!(bytes.chunks(4).all(|p| p[0] == 200 && p[3] == 255));
    }

    #[test]
    fn roundtrip_write_then_read() {
        let dir = std::env::temp_dir().join(format!("mfst_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
