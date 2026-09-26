//! Content-hash-keyed disk cache for expensive CPU decodes.
//!
//! Caches the outputs of `node.hdri_source`'s EXR decode and
//! `node.gltf_mesh_source`'s glTF parse/flatten step under
//! `~/Library/Caches/com.latentspace.manifold/decode_cache/`. The cache key is
//! a SHA-256 of the source file bytes, not the path, so the same path with new
//! content is a guaranteed miss. A cache hit never records a cold touch; a miss
//! records one, keeping the warmup cold-touch detector honest.
//!
//! The cache is disk-only shared state. A stable per-root file lock serializes
//! manifest reconciliation, access touches, writes, and eviction across
//! threads and processes; payloads and manifests are still published through
//! unique temporary files and atomic renames.
//!
//! Corrupted or unverifiable entries are bypassed and re-decoded cold — the
//! caller never sees partial cache data.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
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
/// MeshVertex now carries UV1, corrected transforms and RGBA vertex colour.
/// Separate keys let older app processes keep their own compatible cache.
const MESH_CACHE_VERSION: u32 = 2;

/// Magic header: "MANIFOLD DECODE CACHE" shortened to four bytes.
const MAGIC: &[u8; 4] = b"MDC1";

/// Total cache size cap across all namespaces. Start conservative: 2 GB.
/// The manifest stores the live total and eviction drops oldest `last_accessed`
/// entries until the cache fits, including during read reconciliation.
const TOTAL_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Manifest file name in the cache root.
const MANIFEST_NAME: &str = "manifest.json";

/// Stable per-root lock. It is opened without truncation and held across
/// reconciliation, payload replacement, eviction, and manifest publication.
const LOCK_NAME: &str = ".lock";
const MANAGED_NAMESPACES: [&str; 2] = ["hdri", "gltf_mesh"];

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Self::default();
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Self::default();
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    fn write(&self, root: &Path) -> Result<(), String> {
        let path = root.join(MANIFEST_NAME);
        let bytes = serde_json::to_vec(self).map_err(|e| format!("manifest serialize: {e}"))?;
        atomic_replace_file(root, &path, &bytes, "manifest")?;
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size_bytes).sum()
    }

    fn touch(&mut self, namespace: &str, file_name: &str, size_bytes: u64) {
        let now = now_secs();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.namespace == namespace && e.file_name == file_name)
        {
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

fn is_managed_namespace(namespace: &str) -> bool {
    MANAGED_NAMESPACES.contains(&namespace)
}

fn is_payload_name(file_name: &str) -> bool {
    file_name.len() == 64 && file_name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ensure_real_dir(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{label} is a symlink: {}", path.display()))
        }
        Ok(metadata) if !metadata.is_dir() => {
            Err(format!("{label} is not a directory: {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|e| format!("create {label}: {e}"))?;
            Ok(())
        }
        Err(error) => Err(format!("stat {label}: {error}")),
    }
}

fn ensure_cache_root(root: &Path) -> Result<(), String> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("cache root is a symlink: {}", root.display()))
        }
        Ok(metadata) if !metadata.is_dir() => {
            Err(format!("cache root is not a directory: {}", root.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(|e| format!("create cache root: {e}"))?;
            ensure_real_dir(root, "cache root")
        }
        Err(error) => Err(format!("stat cache root: {error}")),
    }
}

fn ensure_namespace_dir(root: &Path, namespace: &str) -> Result<PathBuf, String> {
    if !is_managed_namespace(namespace) {
        return Err(format!("unmanaged cache namespace: {namespace}"));
    }
    ensure_cache_root(root)?;
    let path = namespace_dir(root, namespace);
    ensure_real_dir(&path, "cache namespace")?;
    Ok(path)
}

struct CacheLock {
    _file: File,
}

fn acquire_cache_lock(root: &Path) -> Result<CacheLock, String> {
    ensure_cache_root(root)?;
    let path = root.join(LOCK_NAME);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!("cache lock is a symlink: {}", path.display()));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(format!("cache lock is not a file: {}", path.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("stat cache lock: {error}")),
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("open cache lock: {e}"))?;
    file.lock().map_err(|e| format!("lock cache root: {e}"))?;
    Ok(CacheLock { _file: file })
}

fn unique_temp_path(root: &Path, prefix: &str) -> PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    root.join(format!(".{prefix}.{}.{}.tmp", std::process::id(), counter))
}

fn atomic_replace_file(
    root: &Path,
    destination: &Path,
    bytes: &[u8],
    prefix: &str,
) -> Result<(), String> {
    ensure_real_dir(root, "cache temporary directory")?;
    let temporary = unique_temp_path(root, prefix);
    atomic_replace_file_at(root, destination, bytes, prefix, temporary)
}

fn atomic_replace_file_at(
    root: &Path,
    destination: &Path,
    bytes: &[u8],
    prefix: &str,
    temporary: PathBuf,
) -> Result<(), String> {
    ensure_real_dir(root, "cache temporary directory")?;
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "refusing to replace symlink destination: {}",
                destination.display()
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(format!(
                "refusing to replace non-file destination: {}",
                destination.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("stat destination: {error}")),
    }
    let mut owns_temporary = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| format!("{prefix} temp create: {e}"))?;
        owns_temporary = true;
        file.write_all(bytes)
            .map_err(|e| format!("{prefix} temp write: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("{prefix} temp sync: {e}"))?;
        fs::rename(&temporary, destination).map_err(|e| format!("{prefix} rename: {e}"))
    })();
    if result.is_err() && owns_temporary {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("{}: read error: {e}", path.display()))?;
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
    if namespace == "gltf_mesh" {
        hasher.update(MESH_CACHE_VERSION.to_le_bytes());
    }
    hasher.update(file_hash);
    hasher.update(extra);
    hex(&hasher.finalize())
}

// Hit/miss counts are thread-scoped on purpose: `node.gltf_mesh_source`
// parses GLBs on detached threads that can complete during unrelated code
// (in tests, during later test cases), and those out-of-band loads must not
// land in whatever thread is asserting on its own counts. A count therefore
// describes loads initiated by the recording thread only.
thread_local! {
    static HDRI_HITS: Cell<u64> = const { Cell::new(0) };
    static HDRI_MISSES: Cell<u64> = const { Cell::new(0) };
    static GLTF_MESH_HITS: Cell<u64> = const { Cell::new(0) };
    static GLTF_MESH_MISSES: Cell<u64> = const { Cell::new(0) };
}

fn record_hdri_hit() {
    HDRI_HITS.with(|c| c.set(c.get() + 1));
}

fn record_hdri_miss() {
    HDRI_MISSES.with(|c| c.set(c.get() + 1));
}

fn record_gltf_mesh_hit() {
    GLTF_MESH_HITS.with(|c| c.set(c.get() + 1));
}

fn record_gltf_mesh_miss() {
    GLTF_MESH_MISSES.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
fn hdri_hits() -> u64 {
    HDRI_HITS.with(Cell::get)
}

#[cfg(test)]
fn hdri_misses() -> u64 {
    HDRI_MISSES.with(Cell::get)
}

#[cfg(test)]
fn gltf_mesh_hits() -> u64 {
    GLTF_MESH_HITS.with(Cell::get)
}

#[cfg(test)]
fn gltf_mesh_misses() -> u64 {
    GLTF_MESH_MISSES.with(Cell::get)
}

/// Selector string used in the cache key and stored in the mesh cache header.
fn mesh_selector_key(selector: &GltfMeshSelector) -> String {
    match selector {
        GltfMeshSelector::WholeScene => "whole".to_string(),
        GltfMeshSelector::Mesh { mesh_index } => format!("mesh:{mesh_index}"),
        GltfMeshSelector::Primitive {
            mesh_index,
            primitive_index,
        } => {
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

    if let Some(ref root) = root
        && let Some(result) = read_hdri_cache_with_root(root, &key, &file_hash)
    {
        record_hdri_hit();
        return Ok(result);
    }

    record_hdri_miss();
    record_cold_touch(ColdTouchKind::HdriDecode);
    let decoded = load_hdri_uncached(path)?;

    if let Some(ref root) = root
        && let Err(e) = write_hdri_cache(root, "hdri", &key, &file_hash, &decoded)
    {
        log::warn!(
            "decode_cache: failed to write HDRI cache for {}: {e}",
            path.display()
        );
    }

    Ok(decoded)
}

fn read_hdri_cache_with_root(
    root: &Path,
    key: &str,
    file_hash: &[u8; 32],
) -> Option<(u32, u32, Vec<u8>)> {
    let _lock = match acquire_cache_lock(root) {
        Ok(lock) => lock,
        Err(error) => {
            log::warn!("decode_cache: failed to lock HDRI cache: {error}");
            return None;
        }
    };
    let mut manifest = match reconcile_locked(root, TOTAL_CACHE_CAP_BYTES) {
        Ok(manifest) => manifest,
        Err(error) => {
            log::warn!("decode_cache: failed to reconcile HDRI cache: {error}");
            return None;
        }
    };
    let entry_path = namespace_dir(root, "hdri").join(key);
    if !matches!(payload_size(&entry_path), Ok(Some(_))) {
        return None;
    }
    let result = read_hdri_cache(&entry_path, file_hash)?;
    if let Ok(metadata) = fs::symlink_metadata(&entry_path)
        && metadata.is_file()
    {
        manifest.touch("hdri", key, metadata.len());
        if let Err(error) = manifest.write(root) {
            log::warn!("decode_cache: failed to record HDRI cache access: {error}");
        }
    }
    Some(result)
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

    if let Some(ref root) = root
        && let Some(result) = read_mesh_cache_with_root(root, &key, &file_hash, &selector_str)
    {
        record_gltf_mesh_hit();
        return Ok(result);
    }

    record_gltf_mesh_miss();
    record_cold_touch(ColdTouchKind::GlbParse);
    let verts = load_gltf_mesh_uncached(path, selector)?;

    if let Some(ref root) = root
        && let Err(e) = write_mesh_cache(root, "gltf_mesh", &key, &file_hash, &selector_str, &verts)
    {
        log::warn!(
            "decode_cache: failed to write glTF mesh cache for {}: {e}",
            path.display()
        );
    }

    Ok(verts)
}

fn read_mesh_cache_with_root(
    root: &Path,
    key: &str,
    file_hash: &[u8; 32],
    selector_str: &str,
) -> Option<Vec<MeshVertex>> {
    let _lock = match acquire_cache_lock(root) {
        Ok(lock) => lock,
        Err(error) => {
            log::warn!("decode_cache: failed to lock glTF mesh cache: {error}");
            return None;
        }
    };
    let mut manifest = match reconcile_locked(root, TOTAL_CACHE_CAP_BYTES) {
        Ok(manifest) => manifest,
        Err(error) => {
            log::warn!("decode_cache: failed to reconcile glTF mesh cache: {error}");
            return None;
        }
    };
    let entry_path = namespace_dir(root, "gltf_mesh").join(key);
    if !matches!(payload_size(&entry_path), Ok(Some(_))) {
        return None;
    }
    let result = read_mesh_cache(&entry_path, file_hash, selector_str)?;
    if let Ok(metadata) = fs::symlink_metadata(&entry_path)
        && metadata.is_file()
    {
        manifest.touch("gltf_mesh", key, metadata.len());
        if let Err(error) = manifest.write(root) {
            log::warn!("decode_cache: failed to record glTF mesh cache access: {error}");
        }
    }
    Some(result)
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
    if version != MESH_CACHE_VERSION {
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
    out.extend_from_slice(&MESH_CACHE_VERSION.to_le_bytes());
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

fn metadata_mtime_secs(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_else(now_secs)
}

/// Return the size of a validated payload file. Symlinks, directories, and
/// other unexpected paths are intentionally ignored and never followed.
fn payload_size(path: &Path) -> Result<Option<(u64, u64)>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("stat cache payload: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some((metadata.len(), metadata_mtime_secs(&metadata))))
}

/// Remove only a validated, exact payload path. A failed unlink leaves the
/// manifest entry in place so the failed deletion is never counted as space
/// reclaimed.
fn remove_payload(root: &Path, entry: &ManifestEntry) -> Result<bool, String> {
    if !is_managed_namespace(&entry.namespace) || !is_payload_name(&entry.file_name) {
        return Err("refusing to remove unmanaged cache path".to_string());
    }
    let namespace_path = namespace_dir(root, &entry.namespace);
    match fs::symlink_metadata(&namespace_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "cache namespace is a symlink: {}",
                namespace_path.display()
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(format!(
                "cache namespace is not a directory: {}",
                namespace_path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("stat cache namespace: {error}")),
    }
    let path = namespace_path.join(&entry.file_name);
    let Some(_) = payload_size(&path)? else {
        return Ok(false);
    };
    fs::remove_file(&path).map_err(|e| format!("remove cache payload: {e}"))?;
    Ok(true)
}

fn evict_to_cap(root: &Path, manifest: &mut Manifest, cap: u64) -> Result<bool, String> {
    let mut changed = false;
    while manifest.total_bytes() > cap {
        let mut indices: Vec<usize> = (0..manifest.entries.len()).collect();
        indices.sort_by_key(|&index| manifest.entries[index].last_accessed);
        let mut progress = false;
        let mut deletion_error = None;
        for index in indices {
            let entry = manifest.entries[index].clone();
            match remove_payload(root, &entry) {
                Ok(_) => {
                    manifest.entries.swap_remove(index);
                    changed = true;
                    progress = true;
                    break;
                }
                Err(error) => deletion_error = Some(error),
            }
        }
        if !progress {
            return Err(deletion_error.unwrap_or_else(|| {
                "cache exceeds size cap and no payload can be evicted".to_string()
            }));
        }
    }
    Ok(changed)
}

/// Rebuild the managed portion of the manifest from real directory metadata,
/// then heal an oversized warm cache. Manifest paths are accepted only when
/// they name a strict payload hash and the corresponding path is a regular
/// file. Valid orphan payloads are indexed using their file modification time.
fn reconcile_locked(root: &Path, cap: u64) -> Result<Manifest, String> {
    ensure_cache_root(root)?;
    let mut manifest = Manifest::read(root);
    let original = manifest.entries.clone();
    let mut retained = Vec::with_capacity(manifest.entries.len());
    for entry in manifest.entries.drain(..) {
        if !is_managed_namespace(&entry.namespace) || !is_payload_name(&entry.file_name) {
            continue;
        }
        let path = namespace_dir(root, &entry.namespace).join(&entry.file_name);
        let Some((size, _)) = payload_size(&path)? else {
            continue;
        };
        if size == entry.size_bytes
            && !retained.iter().any(|existing: &ManifestEntry| {
                existing.namespace == entry.namespace && existing.file_name == entry.file_name
            })
        {
            retained.push(entry);
        }
    }
    manifest.entries = retained;

    for namespace in MANAGED_NAMESPACES {
        let directory = ensure_namespace_dir(root, namespace)?;
        for item in fs::read_dir(&directory).map_err(|e| format!("read cache namespace: {e}"))? {
            let item = item.map_err(|e| format!("read cache namespace entry: {e}"))?;
            let name = item.file_name().to_string_lossy().into_owned();
            if !is_payload_name(&name) {
                continue;
            }
            let path = directory.join(&name);
            let Some((size, modified)) = payload_size(&path)? else {
                continue;
            };
            if !manifest
                .entries
                .iter()
                .any(|entry| entry.namespace == namespace && entry.file_name == name)
            {
                manifest.entries.push(ManifestEntry {
                    namespace: namespace.to_string(),
                    file_name: name,
                    size_bytes: size,
                    last_accessed: modified,
                });
            }
        }
    }

    let evicted = evict_to_cap(root, &mut manifest, cap)?;
    if evicted || manifest.entries != original {
        manifest.write(root)?;
    }
    Ok(manifest)
}

fn write_cache_file(root: &Path, namespace: &str, key: &str, data: &[u8]) -> Result<(), String> {
    write_cache_file_with_cap(root, namespace, key, data, TOTAL_CACHE_CAP_BYTES)
}

fn write_cache_file_with_cap(
    root: &Path,
    namespace: &str,
    key: &str,
    data: &[u8],
    cap: u64,
) -> Result<(), String> {
    if !is_payload_name(key) {
        return Err("refusing to write an invalid cache payload name".to_string());
    }
    let _lock = acquire_cache_lock(root)?;
    let ns_dir = ensure_namespace_dir(root, namespace)?;
    let mut manifest = reconcile_locked(root, cap)?;
    let needed = data.len() as u64;
    if needed > cap {
        return Err(format!("cache payload exceeds size cap ({needed} > {cap})"));
    }

    // Remove the old accounting before fitting the replacement. The old file
    // remains in place until the atomic rename succeeds.
    manifest
        .entries
        .retain(|entry| !(entry.namespace == namespace && entry.file_name == key));
    while manifest.total_bytes().saturating_add(needed) > cap {
        let mut indices: Vec<usize> = (0..manifest.entries.len()).collect();
        indices.sort_by_key(|&index| manifest.entries[index].last_accessed);
        let mut progress = false;
        let mut deletion_error = None;
        for index in indices {
            let entry = manifest.entries[index].clone();
            match remove_payload(root, &entry) {
                Ok(_) => {
                    manifest.entries.swap_remove(index);
                    progress = true;
                    break;
                }
                Err(error) => deletion_error = Some(error),
            }
        }
        if !progress {
            return Err(deletion_error.unwrap_or_else(|| {
                "cache exceeds size cap and no payload can be evicted".to_string()
            }));
        }
    }

    let final_path = ns_dir.join(key);
    atomic_replace_file(&ns_dir, &final_path, data, key)?;

    let size = final_path
        .metadata()
        .map_err(|e| format!("cache metadata: {e}"))?
        .len();
    manifest.touch(namespace, key, size);
    manifest.write(root)?;
    Ok(())
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
    use std::ops::Deref;

    struct TempCacheRoot {
        path: PathBuf,
    }

    impl Deref for TempCacheRoot {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl Drop for TempCacheRoot {
        fn drop(&mut self) {
            // Test cleanup is deliberately bounded and explicit. Unknown
            // paths, symlinks, and non-empty directories are left in place.
            for name in [MANIFEST_NAME, LOCK_NAME] {
                remove_test_file(&self.path.join(name));
            }
            if let Ok(entries) = fs::read_dir(&self.path) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if is_test_temp_name(&name) {
                        remove_test_file(&entry.path());
                    } else if ![MANIFEST_NAME, LOCK_NAME, "hdri", "gltf_mesh", "source"]
                        .contains(&name.as_str())
                    {
                        eprintln!(
                            "decode cache test cleanup left unknown path: {}",
                            entry.path().display()
                        );
                    }
                }
            }
            cleanup_test_namespace(&self.path, "hdri");
            cleanup_test_namespace(&self.path, "gltf_mesh");
            cleanup_test_source(&self.path);
            for name in ["hdri", "gltf_mesh", "source"] {
                remove_test_dir_if_empty(&self.path.join(name));
            }
            remove_test_dir_if_empty(&self.path);
        }
    }

    fn remove_test_file(path: &Path) {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                eprintln!("decode cache test cleanup left symlink: {}", path.display());
            }
            Ok(metadata) if metadata.is_file() => {
                if let Err(error) = fs::remove_file(path) {
                    eprintln!(
                        "decode cache test cleanup failed for {}: {error}",
                        path.display()
                    );
                }
            }
            Ok(_) => eprintln!(
                "decode cache test cleanup left unexpected path: {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "decode cache test cleanup could not inspect {}: {error}",
                path.display()
            ),
        }
    }

    fn remove_test_dir_if_empty(path: &Path) {
        match fs::remove_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("decode cache test cleanup left {}: {error}", path.display()),
        }
    }

    fn cleanup_test_namespace(root: &Path, namespace: &str) {
        let path = root.join(namespace);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                eprintln!(
                    "decode cache test cleanup left namespace symlink: {}",
                    path.display()
                );
                return;
            }
            Ok(metadata) if !metadata.is_dir() => {
                eprintln!(
                    "decode cache test cleanup left non-directory namespace: {}",
                    path.display()
                );
                return;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                eprintln!(
                    "decode cache test cleanup could not inspect namespace {}: {error}",
                    path.display()
                );
                return;
            }
        }
        let Ok(entries) = fs::read_dir(&path) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_payload_name(&name) || is_test_temp_name(&name) {
                remove_test_file(&entry.path());
            } else {
                eprintln!(
                    "decode cache test cleanup left unknown path: {}",
                    entry.path().display()
                );
            }
        }
    }

    fn is_test_temp_name(name: &str) -> bool {
        let parts: Vec<&str> = name.split('.').collect();
        parts.len() == 5
            && parts[0].is_empty()
            && (parts[1] == "manifest" || is_payload_name(parts[1]))
            && parts[4] == "tmp"
            && parts[2].parse::<u32>().is_ok()
            && parts[3].parse::<u64>().is_ok()
    }

    fn cleanup_test_source(root: &Path) {
        let path = root.join("source");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                eprintln!(
                    "decode cache test cleanup left source symlink: {}",
                    path.display()
                );
                return;
            }
            Ok(metadata) if !metadata.is_dir() => {
                eprintln!(
                    "decode cache test cleanup left non-directory source: {}",
                    path.display()
                );
                return;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                eprintln!(
                    "decode cache test cleanup could not inspect source {}: {error}",
                    path.display()
                );
                return;
            }
        }
        let Ok(entries) = fs::read_dir(&path) else {
            return;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry.file_name() == "fixture.exr" {
                remove_test_file(&entry_path);
            } else {
                eprintln!(
                    "decode cache test cleanup left unknown path: {}",
                    entry_path.display()
                );
            }
        }
    }

    fn temp_cache_root() -> TempCacheRoot {
        let dir = std::env::temp_dir().join(format!(
            "manifold-decode-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        TempCacheRoot { path: dir }
    }

    fn reset_counters() {
        HDRI_HITS.with(|c| c.set(0));
        HDRI_MISSES.with(|c| c.set(0));
        GLTF_MESH_HITS.with(|c| c.set(0));
        GLTF_MESH_MISSES.with(|c| c.set(0));
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
        reset_counters();
        let root = temp_cache_root();
        let dir = root.join("source");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        write_synthetic_exr(&path, [1.0, 2.0, 3.0]);

        let (w1, h1, b1) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();
        let (w2, h2, b2) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();

        assert_eq!((w1, h1), (64, 32));
        assert_eq!((w1, h1), (w2, h2));
        assert_eq!(b1, b2);
        assert_eq!(hdri_hits(), 1, "second decode must hit cache");
        assert_eq!(hdri_misses(), 1, "first decode must miss");
    }

    #[test]
    fn hdri_corrupt_cache_entry_re_decodes_and_recovers() {
        reset_counters();
        let root = temp_cache_root();
        let dir = root.join("source");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        write_synthetic_exr(&path, [0.5, 0.25, 0.125]);

        let (_, _, b1) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();

        // Corrupt the on-disk cache file.
        let file_hash = sha256_file(&path).unwrap();
        let key = key_hash("hdri", &file_hash, &[]);
        let cache_path = namespace_dir(&root, "hdri").join(&key);
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(&cache_path)
                .unwrap();
            f.write_all(b"garbage").unwrap();
        }

        let (_, _, b2) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();
        assert_eq!(b1, b2, "re-decoded bytes must match the original");
        assert!(
            hdri_misses() >= 2,
            "corrupt entry must count as a miss + re-decode"
        );
    }

    #[test]
    fn hdri_same_path_different_content_is_a_miss() {
        reset_counters();
        let root = temp_cache_root();
        let dir = root.join("source");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        write_synthetic_exr(&path, [1.0, 0.0, 0.0]);

        let (_, _, b1) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();
        // Rewrite the same path with different content.
        write_synthetic_exr(&path, [0.0, 1.0, 0.0]);
        let (_, _, b2) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();

        assert_ne!(b1, b2, "different content must produce different pixels");
        assert_eq!(
            hdri_misses(),
            2,
            "content change must miss, not reuse stale cache"
        );
    }

    #[test]
    fn gltf_mesh_second_decode_is_cache_hit() {
        reset_counters();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
        if !path.exists() {
            println!("gltf_mesh_second_decode_is_cache_hit: fixture missing, skipping");
            return;
        }
        let root = temp_cache_root();

        let v1 = cached_load_gltf_mesh_with_root(
            &path,
            GltfMeshSelector::WholeScene,
            Some(root.to_path_buf()),
        )
        .unwrap();
        let v2 = cached_load_gltf_mesh_with_root(
            &path,
            GltfMeshSelector::WholeScene,
            Some(root.to_path_buf()),
        )
        .unwrap();

        assert_eq!(v1.len(), v2.len());
        assert!(!v1.is_empty(), "fixture should produce vertices");
        assert_eq!(gltf_mesh_hits(), 1);
        assert_eq!(gltf_mesh_misses(), 1);
    }

    #[test]
    fn gltf_mesh_corrupt_cache_entry_re_decodes_and_recovers() {
        reset_counters();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
        if !path.exists() {
            println!(
                "gltf_mesh_corrupt_cache_entry_re_decodes_and_recovers: fixture missing, skipping"
            );
            return;
        }
        let root = temp_cache_root();
        let selector = GltfMeshSelector::WholeScene;

        let v1 =
            cached_load_gltf_mesh_with_root(&path, selector, Some(root.to_path_buf())).unwrap();

        let file_hash = sha256_file(&path).unwrap();
        let key = key_hash(
            "gltf_mesh",
            &file_hash,
            mesh_selector_key(&selector).as_bytes(),
        );
        let cache_path = namespace_dir(&root, "gltf_mesh").join(&key);
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(&cache_path)
                .unwrap();
            f.write_all(b"not a mesh").unwrap();
        }

        let v2 =
            cached_load_gltf_mesh_with_root(&path, selector, Some(root.to_path_buf())).unwrap();
        assert_eq!(v1.len(), v2.len());
        assert!(gltf_mesh_misses() >= 2);
    }

    #[test]
    fn gltf_mesh_different_selector_is_a_miss() {
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
            Some(root.to_path_buf()),
        )
        .unwrap();
        let _ = cached_load_gltf_mesh_with_root(
            &path,
            GltfMeshSelector::Mesh { mesh_index: 0 },
            Some(root.to_path_buf()),
        )
        .unwrap();

        assert_eq!(
            gltf_mesh_misses(),
            2,
            "different selectors must not share an entry"
        );
        assert_eq!(gltf_mesh_hits(), 0);
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
        let (w1, h1, _) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();
        let cold_ms = cold.elapsed().as_millis();

        let warm = std::time::Instant::now();
        let (w2, h2, _) = cached_load_hdri_with_root(&path, Some(root.to_path_buf())).unwrap();
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

    #[test]
    fn reconcile_recovers_orphan_payload_and_corrupt_manifest() {
        let root = temp_cache_root();
        let key = "a".repeat(64);
        let path = namespace_dir(&root, "hdri").join(&key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"orphan payload").unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"preserve").unwrap();
        fs::write(
            root.join(MANIFEST_NAME),
            br#"{"entries":[{"namespace":"../../outside","file_name":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size_bytes":7,"last_accessed":1}]}"#,
        )
        .unwrap();

        let _lock = acquire_cache_lock(&root).unwrap();
        let manifest = reconcile_locked(&root, 1024).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.entries[0].file_name, key);
        assert_eq!(manifest.entries[0].size_bytes, 14);
        assert!(
            outside.exists(),
            "malformed manifest path must not be deleted"
        );
        drop(_lock);
        fs::remove_file(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reconcile_refuses_traversal_and_symlink_payloads() {
        use std::os::unix::fs::symlink;

        let root = temp_cache_root();
        let key = "b".repeat(64);
        let namespace = namespace_dir(&root, "hdri");
        fs::create_dir_all(&namespace).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, namespace.join(&key)).unwrap();

        assert!(write_cache_file_with_cap(&root, "hdri", "../escape", b"x", 16).is_err());
        let _lock = acquire_cache_lock(&root).unwrap();
        let manifest = reconcile_locked(&root, 1024).unwrap();
        assert!(manifest.entries.is_empty(), "symlink must not be indexed");
        drop(_lock);
        fs::remove_file(namespace.join(&key)).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[test]
    fn cache_budget_handles_replacement_eviction_and_oversize() {
        let root = temp_cache_root();
        let first = "c".repeat(64);
        let second = "d".repeat(64);
        write_cache_file_with_cap(&root, "hdri", &first, b"1234", 8).unwrap();
        write_cache_file_with_cap(&root, "hdri", &first, b"123456", 8).unwrap();
        assert_eq!(Manifest::read(&root).total_bytes(), 6);
        write_cache_file_with_cap(&root, "hdri", &second, b"5678", 8).unwrap();
        assert!(!namespace_dir(&root, "hdri").join(&first).exists());
        let oversize = "e".repeat(64);
        assert!(write_cache_file_with_cap(&root, "hdri", &oversize, b"123456789", 8).is_err());
        assert!(namespace_dir(&root, "hdri").join(&second).exists());
    }

    #[test]
    fn concurrent_writers_leave_manifest_accounting_within_cap() {
        let root = temp_cache_root();
        let mut workers = Vec::new();
        for index in 0..8u8 {
            let root = root.to_path_buf();
            workers.push(std::thread::spawn(move || {
                let key = format!("{index:02x}").repeat(32);
                write_cache_file_with_cap(&root, "hdri", &key, &[index; 4], 16).unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let _lock = acquire_cache_lock(&root).unwrap();
        let manifest = reconcile_locked(&root, 16).unwrap();
        assert!(manifest.total_bytes() <= 16);
        assert!(manifest.entries.iter().all(|entry| {
            namespace_dir(&root, &entry.namespace)
                .join(&entry.file_name)
                .is_file()
        }));
    }

    #[test]
    fn temp_cache_root_cleans_up_on_success_and_unwind() {
        let success_path = {
            let root = temp_cache_root();
            let path = root.to_path_buf();
            write_cache_file_with_cap(&path, "hdri", &"e".repeat(64), b"fixture", 64).unwrap();
            path
        };
        assert!(
            !success_path.exists(),
            "successful test root must be removed"
        );

        let unwind_path = {
            let root = temp_cache_root();
            let path = root.to_path_buf();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _keep_root_alive = root;
                panic!("exercise RAII cleanup");
            }));
            path
        };
        assert!(!unwind_path.exists(), "unwound test root must be removed");
    }

    #[test]
    fn atomic_replace_collision_preserves_preexisting_temp_file() {
        let root = temp_cache_root();
        let temporary = root.join(".collision.temporary.tmp");
        let destination = root.join("destination");
        fs::write(&temporary, b"sentinel").unwrap();

        let result = atomic_replace_file_at(
            &root,
            &destination,
            b"replacement",
            "collision",
            temporary.clone(),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"sentinel");
        fs::remove_file(temporary).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_refuses_destination_symlink() {
        use std::os::unix::fs::symlink;

        let root = temp_cache_root();
        let outside = root.join("outside");
        let destination = root.join("destination");
        fs::write(&outside, b"preserve").unwrap();
        symlink(&outside, &destination).unwrap();

        let result = atomic_replace_file(&root, &destination, b"replacement", "destination");
        assert!(result.is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"preserve");
        assert!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(destination).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn temp_cleanup_does_not_follow_namespace_or_source_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_cache_root();
        let external_namespace = root.join("external_namespace");
        let external_source = root.join("external_source");
        fs::create_dir(&external_namespace).unwrap();
        fs::create_dir(&external_source).unwrap();
        let payload = external_namespace.join("f".repeat(64));
        let source = external_source.join("fixture.exr");
        fs::write(&payload, b"preserve").unwrap();
        fs::write(&source, b"preserve").unwrap();
        symlink(&external_namespace, root.join("hdri")).unwrap();
        symlink(&external_source, root.join("source")).unwrap();
        let root_path = root.to_path_buf();
        drop(root);

        assert!(payload.exists());
        assert!(source.exists());
        fs::remove_file(root_path.join("hdri")).unwrap();
        fs::remove_file(root_path.join("source")).unwrap();
        fs::remove_file(payload).unwrap();
        fs::remove_file(source).unwrap();
        fs::remove_dir(external_namespace).unwrap();
        fs::remove_dir(external_source).unwrap();
        fs::remove_dir(root_path).unwrap();
    }
}
