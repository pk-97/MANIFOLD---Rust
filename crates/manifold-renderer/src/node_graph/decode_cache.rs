//! Content-hash-keyed disk cache for expensive CPU decodes.
//!
//! Caches the outputs of `node.hdri_source`'s EXR decode and
//! `node.gltf_mesh_source`'s glTF parse/flatten step under
//! `~/Library/Caches/com.latentspace.manifold/decode_cache/`. The cache key is
//! a SHA-256 of the source file bytes, not the path, so the same path with new
//! content is a guaranteed miss. A cache hit never records a cold touch; a miss
//! records one, keeping the warmup cold-touch detector honest.
//!
//! The cache is intentionally disk-only shared state: there is no in-memory
//! lock. Concurrent reads/writes inside one process (or across processes) can
//! race on the LRU manifest, but each payload file is written to a temp and
//! atomically renamed, so a race can only waste a redundant decode, never
//! produce a partially-written cache read. Cross-process locking is left out
//! because the cache is per-process and the brief says to note, not build, that
//! complexity.
//!
//! Corrupted or unverifiable entries are deleted and re-decoded cold — the
//! caller never sees partial cache data.

use std::fs;
use std::io::Read;
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use manifold_foundation::cold_touch::{ColdTouchKind, record_cold_touch};
use sha2::{Digest, Sha256};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::gltf_load::{GltfMeshSelector, load_gltf_mesh as load_gltf_mesh_uncached};
use crate::node_graph::primitives::hdri_source::load_hdri as load_hdri_uncached;

/// On-disk format version. Bumped whenever the header or payload layout
/// changes so old entries are treated as corrupt and re-decoded.
const CACHE_VERSION: u32 = 1;

/// Magic header: "MANIFOLD DECODE CACHE" shortened to four bytes.
const MAGIC: &[u8; 4] = b"MDC1";

/// Total cache size cap across all namespaces. Start conservative: 2 GB.
/// This is a soft cap per write; reads never evict. The manifest stores the
/// live total and eviction drops oldest `last_accessed` entries until the new
/// entry fits.
const TOTAL_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Manifest file name in the cache root.
const MANIFEST_NAME: &str = "manifest.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ManifestEntry {
    namespace: String,
    file_name: String,
    size_bytes: u64,
    last_accessed: u64,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Manifest {
    entries: Vec<ManifestEntry>,
}

impl Manifest {
    fn read(root: &Path) -> Self {
        let path = root.join(MANIFEST_NAME);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    fn write(&self, root: &Path) -> Result<(), String> {
        let path = root.join(MANIFEST_NAME);
        let tmp = root.join(format!("{}.tmp", MANIFEST_NAME));
        let bytes = serde_json::to_vec(self).map_err(|e| format!("manifest serialize: {e}"))?;
        fs::write(&tmp, bytes).map_err(|e| format!("manifest temp write: {e}"))?;
        fs::rename(&tmp, &path).map_err(|e| format!("manifest rename: {e}"))?;
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size_bytes).sum()
    }

    fn touch(&mut self, namespace: &str, file_name: &str, size_bytes: u64) {
        let now = now_secs();
        if let Some(entry) = self.entries.iter_mut().find(|e| {
            e.namespace == namespace && e.file_name == file_name
        }) {
            entry.last_accessed = now;
            entry.size_bytes = size_bytes;
        } else {
            self.entries.push(ManifestEntry {
                namespace: namespace.to_string(),
                file_name: file_name.to_string(),
                size_bytes,
                last_accessed: now,
            });
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cache_root() -> Option<PathBuf> {
    // Tests can point the cache elsewhere by calling the `_with_root`
    // variants; production always uses the canonical user cache directory.
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Caches/com.latentspace.manifold/decode_cache"))
}

fn namespace_dir(root: &Path, namespace: &str) -> PathBuf {
    root.join(namespace)
}

fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{}: read error: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn key_hash(namespace: &str, file_hash: &[u8; 32], extra: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update(file_hash);
    hasher.update(extra);
    hex(&hasher.finalize())
}

static HDRI_HITS: AtomicU64 = AtomicU64::new(0);
static HDRI_MISSES: AtomicU64 = AtomicU64::new(0);
static GLTF_MESH_HITS: AtomicU64 = AtomicU64::new(0);
static GLTF_MESH_MISSES: AtomicU64 = AtomicU64::new(0);

fn record_hdri_hit() {
    HDRI_HITS.fetch_add(1, Ordering::Relaxed);
}

fn record_hdri_miss() {
    HDRI_MISSES.fetch_add(1, Ordering::Relaxed);
}

fn record_gltf_mesh_hit() {
    GLTF_MESH_HITS.fetch_add(1, Ordering::Relaxed);
}

fn record_gltf_mesh_miss() {
    GLTF_MESH_MISSES.fetch_add(1, Ordering::Relaxed);
}

/// Selector string used in the cache key and stored in the mesh cache header.
fn mesh_selector_key(selector: &GltfMeshSelector) -> String {
    match selector {
        GltfMeshSelector::WholeScene => "whole".to_string(),
        GltfMeshSelector::Mesh { mesh_index } => format!("mesh:{mesh_index}"),
        GltfMeshSelector::Primitive { mesh_index, primitive_index } => {
            format!("primitive:{mesh_index}:{primitive_index}")
        }
        GltfMeshSelector::Material { material_index } => format!("material:{material_index}"),
        GltfMeshSelector::DefaultMaterial => "default-material".to_string(),
    }
}

/// Load an HDRI through the cache. A miss decodes cold, records the cold
/// touch, and writes the result to disk; a hit returns the cached buffer
/// without touching the detector.
pub(crate) fn cached_load_hdri(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    cached_load_hdri_with_root(path, cache_root())
}

fn cached_load_hdri_with_root(
    path: &Path,
    root: Option<PathBuf>,
) -> Result<(u32, u32, Vec<u8>), String> {
    let file_hash = sha256_file(path)?;
    let key = key_hash("hdri", &file_hash, &[]);

    if let Some(ref root) = root {
        let ns_dir = namespace_dir(root, "hdri");
        fs::create_dir_all(&ns_dir).ok();
        let entry_path = ns_dir.join(&key);
        if let Some(result) = read_hdri_cache(&entry_path, &file_hash) {
            record_hdri_hit();
            touch_entry(root, "hdri", &key, entry_path.metadata().ok().map(|m| m.len()).unwrap_or(0));
            return Ok(result);
        }
    }

    record_hdri_miss();
    record_cold_touch(ColdTouchKind::HdriDecode);
    let decoded = load_hdri_uncached(path)?;

    if let Some(ref root) = root
        && let Err(e) = write_hdri_cache(root, "hdri", &key, &file_hash, &decoded)
    {
        log::warn!("decode_cache: failed to write HDRI cache for {}: {e}", path.display());
    }

    Ok(decoded)
}

fn read_hdri_cache(path: &Path, file_hash: &[u8; 32]) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = fs::read(path).ok()?;
    let mut cursor = &bytes[..];

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let version = read_u32(&mut cursor)?;
    if version != CACHE_VERSION {
        return None;
    }
    let kind = read_u8(&mut cursor)?;
    if kind != 1 {
        return None;
    }
    let stored_file_hash = read_hash(&mut cursor)?;
    if stored_file_hash != *file_hash {
        return None;
    }
    let stored_payload_hash = read_hash(&mut cursor)?;
    let width = read_u32(&mut cursor)?;
    let height = read_u32(&mut cursor)?;
    let payload = cursor.to_vec();
    if sha256_bytes(&payload) != stored_payload_hash {
        return None;
    }

    Some((width, height, payload))
}

fn write_hdri_cache(
    root: &Path,
    namespace: &str,
    key: &str,
    file_hash: &[u8; 32],
    decoded: &(u32, u32, Vec<u8>),
) -> Result<(), String> {
    let (width, height, payload) = decoded;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    out.push(1u8); // kind: HDRI
    out.extend_from_slice(file_hash);
    out.extend_from_slice(&sha256_bytes(payload));
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(payload);

    write_cache_file(root, namespace, key, &out)?;
    Ok(())
}

/// Load a GLB mesh flatten through the cache. The cached payload is the raw
/// `Vec<MeshVertex>` from `load_gltf_mesh` *before* fit/translate; those cheap
/// per-vertex passes are applied by the caller after the cache read. A miss
/// records the GLB-parse cold touch; a hit does not.
pub(crate) fn cached_load_gltf_mesh(
    path: &Path,
    selector: GltfMeshSelector,
) -> Result<Vec<MeshVertex>, String> {
    cached_load_gltf_mesh_with_root(path, selector, cache_root())
}

fn cached_load_gltf_mesh_with_root(
    path: &Path,
    selector: GltfMeshSelector,
    root: Option<PathBuf>,
) -> Result<Vec<MeshVertex>, String> {
    let file_hash = sha256_file(path)?;
    let selector_str = mesh_selector_key(&selector);
    let key = key_hash("gltf_mesh", &file_hash, selector_str.as_bytes());

    if let Some(ref root) = root {
        let ns_dir = namespace_dir(root, "gltf_mesh");
        fs::create_dir_all(&ns_dir).ok();
        let entry_path = ns_dir.join(&key);
        if let Some(result) = read_mesh_cache(&entry_path, &file_hash, &selector_str) {
            record_gltf_mesh_hit();
            touch_entry(root, "gltf_mesh", &key, entry_path.metadata().ok().map(|m| m.len()).unwrap_or(0));
            return Ok(result);
        }
    }

    record_gltf_mesh_miss();
    record_cold_touch(ColdTouchKind::GlbParse);
    let verts = load_gltf_mesh_uncached(path, selector)?;

    if let Some(ref root) = root
        && let Err(e) = write_mesh_cache(root, "gltf_mesh", &key, &file_hash, &selector_str, &verts)
    {
        log::warn!("decode_cache: failed to write glTF mesh cache for {}: {e}", path.display());
    }

    Ok(verts)
}

fn read_mesh_cache(
    path: &Path,
    file_hash: &[u8; 32],
    selector_str: &str,
) -> Option<Vec<MeshVertex>> {
    let bytes = fs::read(path).ok()?;
    let mut cursor = &bytes[..];

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let version = read_u32(&mut cursor)?;
    if version != CACHE_VERSION {
        return None;
    }
    let kind = read_u8(&mut cursor)?;
    if kind != 2 {
        return None;
    }
    let stored_file_hash = read_hash(&mut cursor)?;
    if stored_file_hash != *file_hash {
        return None;
    }
    let stored_payload_hash = read_hash(&mut cursor)?;
    let stored_selector_len = read_u32(&mut cursor)? as usize;
    if stored_selector_len > cursor.len() {
        return None;
    }
    let stored_selector = std::str::from_utf8(&cursor[..stored_selector_len]).ok()?;
    if stored_selector != selector_str {
        return None;
    }
    cursor = &cursor[stored_selector_len..];
    let vertex_count = read_u64(&mut cursor)? as usize;
    let payload = cursor.to_vec();
    if sha256_bytes(&payload) != stored_payload_hash {
        return None;
    }
    if vertex_count * std::mem::size_of::<MeshVertex>() != payload.len() {
        return None;
    }

    // MeshVertex is Pod, but the byte slice we just read from disk is only
    // byte-aligned. Read each vertex unaligned to avoid bytemuck's alignment
    // check on the whole slice.
    let mut verts = Vec::with_capacity(vertex_count);
    for chunk in payload.chunks_exact(std::mem::size_of::<MeshVertex>()) {
        verts.push(bytemuck::pod_read_unaligned(chunk));
    }

    Some(verts)
}

fn write_mesh_cache(
    root: &Path,
    namespace: &str,
    key: &str,
    file_hash: &[u8; 32],
    selector_str: &str,
    verts: &[MeshVertex],
) -> Result<(), String> {
    let payload = bytemuck::cast_slice(verts);
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    out.push(2u8); // kind: mesh
    out.extend_from_slice(file_hash);
    out.extend_from_slice(&sha256_bytes(payload));
    out.extend_from_slice(&(selector_str.len() as u32).to_le_bytes());
    out.extend_from_slice(selector_str.as_bytes());
    out.extend_from_slice(&(verts.len() as u64).to_le_bytes());
    out.extend_from_slice(payload);

    write_cache_file(root, namespace, key, &out)?;
    Ok(())
}

fn write_cache_file(root: &Path, namespace: &str, key: &str, data: &[u8]) -> Result<(), String> {
    let ns_dir = namespace_dir(root, namespace);
    fs::create_dir_all(&ns_dir).map_err(|e| format!("create dir: {e}"))?;

    let mut manifest = Manifest::read(root);
    // Account for the new entry and evict oldest entries until under cap.
    let needed = data.len() as u64;
    while manifest.total_bytes().saturating_add(needed) > TOTAL_CACHE_CAP_BYTES && !manifest.entries.is_empty() {
        let oldest = manifest.entries.iter().enumerate().min_by_key(|(_, e)| e.last_accessed);
        if let Some((idx, entry)) = oldest {
            let victim = namespace_dir(root, &entry.namespace).join(&entry.file_name);
            let _ = fs::remove_file(&victim);
            manifest.entries.swap_remove(idx);
        } else {
            break;
        }
    }

    let final_path = ns_dir.join(key);
    let tmp_path = ns_dir.join(format!("{key}.tmp"));
    fs::write(&tmp_path, data).map_err(|e| format!("cache temp write: {e}"))?;
    fs::rename(&tmp_path, &final_path).map_err(|e| format!("cache rename: {e}"))?;

    let size = final_path.metadata().map_err(|e| format!("cache metadata: {e}"))?.len();
    manifest.touch(namespace, key, size);
    manifest.write(root)?;
    Ok(())
}

fn touch_entry(root: &Path, namespace: &str, file_name: &str, size_bytes: u64) {
    let mut manifest = Manifest::read(root);
    manifest.touch(namespace, file_name, size_bytes);
    let _ = manifest.write(root);
}

fn read_u8(cursor: &mut &[u8]) -> Option<u8> {
    if cursor.is_empty() {
        return None;
    }
    let v = cursor[0];
    *cursor = &cursor[1..];
    Some(v)
}

fn read_u32(cursor: &mut &[u8]) -> Option<u32> {
    if cursor.len() < 4 {
        return None;
    }
    let v = u32::from_le_bytes(cursor[..4].try_into().unwrap());
    *cursor = &cursor[4..];
    Some(v)
}

fn read_u64(cursor: &mut &[u8]) -> Option<u64> {
    if cursor.len() < 8 {
        return None;
    }
    let v = u64::from_le_bytes(cursor[..8].try_into().unwrap());
    *cursor = &cursor[8..];
    Some(v)
}

fn read_hash(cursor: &mut &[u8]) -> Option<[u8; 32]> {
    if cursor.len() < 32 {
        return None;
    }
    let v = cursor[..32].try_into().unwrap();
    *cursor = &cursor[32..];
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    // Counter tests run under this lock so the process-global hit/miss
    // atomics aren't perturbed by parallel tests.
    static COUNTER_LOCK: Mutex<()> = Mutex::new(());

    fn temp_cache_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "manifold-decode-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn reset_counters() {
        HDRI_HITS.store(0, Ordering::Relaxed);
        HDRI_MISSES.store(0, Ordering::Relaxed);
        GLTF_MESH_HITS.store(0, Ordering::Relaxed);
        GLTF_MESH_MISSES.store(0, Ordering::Relaxed);
    }

    fn write_synthetic_exr(path: &Path, color: [f32; 3]) {
        let mut buf: image::Rgb32FImage = image::ImageBuffer::new(64, 32);
        for px in buf.pixels_mut() {
            *px = image::Rgb(color);
        }
        image::DynamicImage::ImageRgb32F(buf)
            .save_with_format(path, image::ImageFormat::OpenExr)
            .unwrap();
    }

    #[test]
    fn hdri_second_decode_is_cache_hit() {
        let _guard = COUNTER_LOCK.lock().unwrap();
        reset_counters();
        let root = temp_cache_root();
        let dir = root.join("source");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        write_synthetic_exr(&path, [1.0, 2.0, 3.0]);

        let (w1, h1, b1) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();
        let (w2, h2, b2) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();

        assert_eq!((w1, h1), (64, 32));
        assert_eq!((w1, h1), (w2, h2));
        assert_eq!(b1, b2);
        assert_eq!(HDRI_HITS.load(Ordering::Relaxed), 1, "second decode must hit cache");
        assert_eq!(HDRI_MISSES.load(Ordering::Relaxed), 1, "first decode must miss");
    }

    #[test]
    fn hdri_corrupt_cache_entry_re_decodes_and_recovers() {
        let _guard = COUNTER_LOCK.lock().unwrap();
        reset_counters();
        let root = temp_cache_root();
        let dir = root.join("source");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        write_synthetic_exr(&path, [0.5, 0.25, 0.125]);

        let (_, _, b1) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();

        // Corrupt the on-disk cache file.
        let file_hash = sha256_file(&path).unwrap();
        let key = key_hash("hdri", &file_hash, &[]);
        let cache_path = namespace_dir(&root, "hdri").join(&key);
        {
            let mut f = fs::OpenOptions::new().write(true).open(&cache_path).unwrap();
            f.write_all(b"garbage").unwrap();
        }

        let (_, _, b2) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();
        assert_eq!(b1, b2, "re-decoded bytes must match the original");
        assert!(
            HDRI_MISSES.load(Ordering::Relaxed) >= 2,
            "corrupt entry must count as a miss + re-decode"
        );
    }

    #[test]
    fn hdri_same_path_different_content_is_a_miss() {
        let _guard = COUNTER_LOCK.lock().unwrap();
        reset_counters();
        let root = temp_cache_root();
        let dir = root.join("source");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        write_synthetic_exr(&path, [1.0, 0.0, 0.0]);

        let (_, _, b1) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();
        // Rewrite the same path with different content.
        write_synthetic_exr(&path, [0.0, 1.0, 0.0]);
        let (_, _, b2) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();

        assert_ne!(b1, b2, "different content must produce different pixels");
        assert_eq!(HDRI_MISSES.load(Ordering::Relaxed), 2, "content change must miss, not reuse stale cache");
    }

    #[test]
    fn gltf_mesh_second_decode_is_cache_hit() {
        let _guard = COUNTER_LOCK.lock().unwrap();
        reset_counters();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
        if !path.exists() {
            println!("gltf_mesh_second_decode_is_cache_hit: fixture missing, skipping");
            return;
        }
        let root = temp_cache_root();

        let v1 = cached_load_gltf_mesh_with_root(&path, GltfMeshSelector::WholeScene, Some(root.clone())).unwrap();
        let v2 = cached_load_gltf_mesh_with_root(&path, GltfMeshSelector::WholeScene, Some(root.clone())).unwrap();

        assert_eq!(v1.len(), v2.len());
        assert!(!v1.is_empty(), "fixture should produce vertices");
        assert_eq!(GLTF_MESH_HITS.load(Ordering::Relaxed), 1);
        assert_eq!(GLTF_MESH_MISSES.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn gltf_mesh_corrupt_cache_entry_re_decodes_and_recovers() {
        let _guard = COUNTER_LOCK.lock().unwrap();
        reset_counters();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
        if !path.exists() {
            println!("gltf_mesh_corrupt_cache_entry_re_decodes_and_recovers: fixture missing, skipping");
            return;
        }
        let root = temp_cache_root();
        let selector = GltfMeshSelector::WholeScene;

        let v1 = cached_load_gltf_mesh_with_root(&path, selector, Some(root.clone())).unwrap();

        let file_hash = sha256_file(&path).unwrap();
        let key = key_hash("gltf_mesh", &file_hash, mesh_selector_key(&selector).as_bytes());
        let cache_path = namespace_dir(&root, "gltf_mesh").join(&key);
        {
            let mut f = fs::OpenOptions::new().write(true).open(&cache_path).unwrap();
            f.write_all(b"not a mesh").unwrap();
        }

        let v2 = cached_load_gltf_mesh_with_root(&path, selector, Some(root.clone())).unwrap();
        assert_eq!(v1.len(), v2.len());
        assert!(GLTF_MESH_MISSES.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn gltf_mesh_different_selector_is_a_miss() {
        let _guard = COUNTER_LOCK.lock().unwrap();
        reset_counters();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
        if !path.exists() {
            println!("gltf_mesh_different_selector_is_a_miss: fixture missing, skipping");
            return;
        }
        let root = temp_cache_root();

        let _ = cached_load_gltf_mesh_with_root(
            &path,
            GltfMeshSelector::WholeScene,
            Some(root.clone()),
        )
        .unwrap();
        let _ = cached_load_gltf_mesh_with_root(
            &path,
            GltfMeshSelector::Mesh { mesh_index: 0 },
            Some(root.clone()),
        )
        .unwrap();

        assert_eq!(GLTF_MESH_MISSES.load(Ordering::Relaxed), 2, "different selectors must not share an entry");
        assert_eq!(GLTF_MESH_HITS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn kloppenheim_4k_second_decode_is_warm() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/hdri/kloppenheim_07_puresky_4k.exr");
        if !path.exists() {
            println!("kloppenheim_4k_second_decode_is_warm: fixture missing, skipping");
            return;
        }
        let root = temp_cache_root();

        let cold = std::time::Instant::now();
        let (w1, h1, _) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();
        let cold_ms = cold.elapsed().as_millis();

        let warm = std::time::Instant::now();
        let (w2, h2, _) = cached_load_hdri_with_root(&path, Some(root.clone())).unwrap();
        let warm_ms = warm.elapsed().as_millis();

        assert_eq!((w1, h1), (w2, h2));
        println!(
            "kloppenheim_07_puresky_4k.exr decode: cold={cold_ms}ms, warm={warm_ms}ms, dims={w1}x{h1}"
        );
        assert!(
            warm_ms < cold_ms.max(1),
            "warm cache read should be faster than cold decode: cold={cold_ms}ms warm={warm_ms}ms"
        );
    }
}
