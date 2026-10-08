//! On-disk MSL shader cache — skips WGSL → naga → SPIR-V → spirv-opt → SPIRV-Cross
//! on cache hit.
//!
//! Cache key: shader source + entry point(s) + translation options (half
//! compute or render point-size rewrite), supplied by `archive` key helpers.
//! Cache value: compiled MSL source + SlotMap + workgroup size.
//! Stored as one plain-text file per shader in the cache directory.
//! Invalidation is automatic — if WGSL content changes, the hash changes.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{SIZES_BUFFER_BINDING, Slot, SlotKind, SlotMap};

const COMPUTE_HEADER: &str = "MSL_CACHE_V1_COMPUTE";
const RENDER_HEADER: &str = "MSL_CACHE_V1_RENDER";
const MSL_SEPARATOR: &str = "===MSL===";
const VS_SEPARATOR: &str = "===VS===";
const FS_SEPARATOR: &str = "===FS===";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Cached result of a compute shader compilation.
pub(super) struct ComputeCacheEntry {
    pub slot_map: SlotMap,
    pub msl_source: String,
    pub msl_entry_name: String,
    pub workgroup_size: [u32; 3],
}

/// Cached result of a render shader compilation.
pub(super) struct RenderCacheEntry {
    pub slot_map: SlotMap,
    pub vs_msl: String,
    pub fs_msl: String,
}

/// On-disk MSL shader cache.
pub struct MslCache {
    cache_dir: PathBuf,
    hits: u32,
    misses: u32,
}

impl MslCache {
    /// Create or open a cache directory. Creates the directory if it doesn't exist.
    pub fn new(cache_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&cache_dir).ok();
        Self {
            cache_dir,
            hits: 0,
            misses: 0,
        }
    }

    fn path_for(&self, hash: u64) -> PathBuf {
        self.cache_dir.join(format!("{hash:016x}.mslcache"))
    }

    fn put_atomic(&self, hash: u64, write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>) {
        let temp = self.cache_dir.join(format!(
            ".{hash:016x}.{}.{}.tmp", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp) else {
            return;
        };
        let written = write(&mut file);
        drop(file);
        // Readers see either the previous complete entry or the new complete entry.
        if written.is_err() || std::fs::rename(&temp, self.path_for(hash)).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
    }

    /// Look up a cached compute shader compilation result.
    pub(super) fn get_compute(&mut self, hash: u64) -> Option<ComputeCacheEntry> {
        let path = self.path_for(hash);
        let file = std::fs::File::open(&path).ok()?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();

        // Header
        let header = lines.next()?.ok()?;
        if header != COMPUTE_HEADER {
            return None;
        }

        // Slot map
        let slot_map = read_slot_map(&mut lines)?;

        // Workgroup size
        let wg_line = lines.next()?.ok()?;
        let wg: Vec<u32> = wg_line.split(' ').filter_map(|s| s.parse().ok()).collect();
        if wg.len() != 3 {
            return None;
        }
        let workgroup_size = [wg[0], wg[1], wg[2]];

        // Entry name
        let entry_name = lines.next()?.ok()?;

        // MSL separator
        let sep = lines.next()?.ok()?;
        if sep != MSL_SEPARATOR {
            return None;
        }

        // MSL source (rest of file)
        let msl_source: String = lines
            .map(|l| l.unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");

        self.hits += 1;
        Some(ComputeCacheEntry {
            slot_map,
            msl_source,
            msl_entry_name: entry_name,
            workgroup_size,
        })
    }

    /// Store a compute shader compilation result.
    pub(super) fn put_compute(
        &self,
        hash: u64,
        slot_map: &SlotMap,
        msl_source: &str,
        msl_entry_name: &str,
        workgroup_size: [u32; 3],
    ) {
        self.put_atomic(hash, |file| {
            writeln!(file, "{COMPUTE_HEADER}")?;
            write_slot_map(file, slot_map)?;
            writeln!(file, "{} {} {}", workgroup_size[0], workgroup_size[1], workgroup_size[2])?;
            writeln!(file, "{msl_entry_name}")?;
            writeln!(file, "{MSL_SEPARATOR}")?;
            write!(file, "{msl_source}")
        });
    }

    /// Look up a cached render shader compilation result.
    pub(super) fn get_render(&mut self, hash: u64) -> Option<RenderCacheEntry> {
        let path = self.path_for(hash);
        let content = std::fs::read_to_string(&path).ok()?;

        // Header
        if !content.starts_with(RENDER_HEADER) {
            return None;
        }

        // Split into sections
        let vs_start = content.find(VS_SEPARATOR)?;
        let fs_start = content.find(FS_SEPARATOR)?;

        // Parse slot map from header section
        let header_section = &content[RENDER_HEADER.len() + 1..vs_start];
        let slot_map = read_slot_map_from_str(header_section)?;

        // Extract MSL sources
        let vs_msl = content[vs_start + VS_SEPARATOR.len() + 1..fs_start]
            .trim_end()
            .to_string();
        let fs_msl = content[fs_start + FS_SEPARATOR.len() + 1..].to_string();

        self.hits += 1;
        Some(RenderCacheEntry {
            slot_map,
            vs_msl,
            fs_msl,
        })
    }

    /// Store a render shader compilation result.
    pub(super) fn put_render(&self, hash: u64, slot_map: &SlotMap, vs_msl: &str, fs_msl: &str) {
        self.put_atomic(hash, |file| {
            writeln!(file, "{RENDER_HEADER}")?;
            write_slot_map(file, slot_map)?;
            writeln!(file, "{VS_SEPARATOR}")?;
            write!(file, "{vs_msl}")?;
            if !vs_msl.ends_with('\n') {
                writeln!(file)?;
            }
            writeln!(file, "{FS_SEPARATOR}")?;
            write!(file, "{fs_msl}")
        });
    }

    pub(super) fn record_miss(&mut self) {
        self.misses += 1;
    }

    /// Log cache statistics.
    pub fn log_stats(&self) {
        let total = self.hits + self.misses;
        if total > 0 {
            log::info!(
                "[MslCache] {}/{} hits ({} misses)",
                self.hits,
                total,
                self.misses,
            );
        }
    }
}

// ─── SlotMap serialization ───────────────────────────────────────────

fn write_slot_map(file: &mut std::fs::File, slot_map: &SlotMap) -> std::io::Result<()> {
    // Collect all valid slots
    let entries: Vec<_> = (0..=SIZES_BUFFER_BINDING)
        .filter_map(|b| slot_map.get(b).map(|s| (b, s)))
        .collect();
    writeln!(file, "{}", entries.len())?;
    for (binding, slot) in entries {
        let kind_char = match slot.kind {
            SlotKind::Buffer => 'B',
            SlotKind::Texture => 'T',
            SlotKind::Sampler => 'S',
        };
        writeln!(file, "{binding} {kind_char} {}", slot.metal_index)?;
    }
    Ok(())
}

fn read_slot_map(
    lines: &mut impl Iterator<Item = Result<String, std::io::Error>>,
) -> Option<SlotMap> {
    let count_line = lines.next()?.ok()?;
    let count: usize = count_line.trim().parse().ok()?;
    let mut slot_map = SlotMap::new();
    for _ in 0..count {
        let line = lines.next()?.ok()?;
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 3 {
            return None;
        }
        let binding: u32 = parts[0].parse().ok()?;
        let kind = match parts[1] {
            "B" => SlotKind::Buffer,
            "T" => SlotKind::Texture,
            "S" => SlotKind::Sampler,
            _ => return None,
        };
        let metal_index: u32 = parts[2].parse().ok()?;
        slot_map.insert(binding, Slot { kind, metal_index });
    }
    Some(slot_map)
}

fn read_slot_map_from_str(section: &str) -> Option<SlotMap> {
    let mut lines = section.lines();
    let count: usize = lines.next()?.trim().parse().ok()?;
    let mut slot_map = SlotMap::new();
    for _ in 0..count {
        let line = lines.next()?;
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 3 {
            return None;
        }
        let binding: u32 = parts[0].parse().ok()?;
        let kind = match parts[1] {
            "B" => SlotKind::Buffer,
            "T" => SlotKind::Texture,
            "S" => SlotKind::Sampler,
            _ => return None,
        };
        let metal_index: u32 = parts[2].parse().ok()?;
        slot_map.insert(binding, Slot { kind, metal_index });
    }
    Some(slot_map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn puts_round_trip_through_atomic_replacement() {
        let dir = std::env::temp_dir()
            .join(format!("msl-cache-{}-{}", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cache = MslCache::new(dir.clone());
        let mut slots = SlotMap::new();
        slots.insert(0, Slot { kind: SlotKind::Buffer, metal_index: 3 });
        slots.insert(2, Slot { kind: SlotKind::Texture, metal_index: 5 });
        slots.insert(SIZES_BUFFER_BINDING, Slot { kind: SlotKind::Buffer, metal_index: 7 });

        cache.put_compute(1, &slots, "old compute", "main", [8, 4, 2]);
        let old_bytes = std::fs::read(cache.path_for(1)).unwrap();
        let mut old_reader = std::fs::File::open(cache.path_for(1)).unwrap();
        cache.put_compute(1, &slots, "new compute", "next", [16, 2, 1]);
        let mut held_bytes = Vec::new();
        old_reader.read_to_end(&mut held_bytes).unwrap();
        assert_eq!(held_bytes, old_bytes, "replacement must not truncate an open reader");
        let compute = cache.get_compute(1).unwrap();
        assert_eq!(compute.msl_source, "new compute");
        assert_eq!(compute.msl_entry_name, "next");
        assert_eq!(compute.workgroup_size, [16, 2, 1]);

        cache.put_render(2, &slots, "old vertex", "old fragment");
        let old_bytes = std::fs::read(cache.path_for(2)).unwrap();
        let mut old_reader = std::fs::File::open(cache.path_for(2)).unwrap();
        cache.put_render(2, &slots, "new vertex", "new fragment");
        let mut held_bytes = Vec::new();
        old_reader.read_to_end(&mut held_bytes).unwrap();
        assert_eq!(held_bytes, old_bytes, "render replacement must preserve an open reader");
        let render = cache.get_render(2).unwrap();
        assert_eq!(render.vs_msl, "new vertex");
        assert_eq!(render.fs_msl, "new fragment");
        for map in [&compute.slot_map, &render.slot_map] {
            for binding in [0, 2, SIZES_BUFFER_BINDING] {
                let expected = slots.get(binding).unwrap();
                let actual = map.get(binding).unwrap();
                assert_eq!(actual.kind, expected.kind);
                assert_eq!(actual.metal_index, expected.metal_index);
            }
        }
        cache.put_atomic(1, |file| {
            write!(file, "partial")?;
            Err(std::io::Error::other("injected write failure"))
        });
        assert_eq!(cache.get_compute(1).unwrap().msl_source, "new compute");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no temporary files remain");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
