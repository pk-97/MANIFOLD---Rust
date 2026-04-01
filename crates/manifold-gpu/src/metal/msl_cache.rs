//! On-disk MSL shader cache — skips WGSL → naga → SPIR-V → spirv-opt → SPIRV-Cross
//! on cache hit.
//!
//! Cache key: hash of WGSL source + entry point(s).
//! Cache value: compiled MSL source + SlotMap + workgroup size.
//! Stored as one plain-text file per shader in the cache directory.
//! Invalidation is automatic — if WGSL content changes, the hash changes.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use super::{SlotMap, Slot, SlotKind, SIZES_BUFFER_BINDING};

const COMPUTE_HEADER: &str = "MSL_CACHE_V1_COMPUTE";
const RENDER_HEADER: &str = "MSL_CACHE_V1_RENDER";
const MSL_SEPARATOR: &str = "===MSL===";
const VS_SEPARATOR: &str = "===VS===";
const FS_SEPARATOR: &str = "===FS===";

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
        Self { cache_dir, hits: 0, misses: 0 }
    }

    fn path_for(&self, hash: u64) -> PathBuf {
        self.cache_dir.join(format!("{hash:016x}.mslcache"))
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
        let path = self.path_for(hash);
        let Ok(mut file) = std::fs::File::create(&path) else { return };

        let _ = writeln!(file, "{COMPUTE_HEADER}");
        write_slot_map(&mut file, slot_map);
        let _ = writeln!(file, "{} {} {}", workgroup_size[0], workgroup_size[1], workgroup_size[2]);
        let _ = writeln!(file, "{msl_entry_name}");
        let _ = writeln!(file, "{MSL_SEPARATOR}");
        let _ = write!(file, "{msl_source}");
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
        let fs_msl = content[fs_start + FS_SEPARATOR.len() + 1..]
            .to_string();

        self.hits += 1;
        Some(RenderCacheEntry { slot_map, vs_msl, fs_msl })
    }

    /// Store a render shader compilation result.
    pub(super) fn put_render(
        &self,
        hash: u64,
        slot_map: &SlotMap,
        vs_msl: &str,
        fs_msl: &str,
    ) {
        let path = self.path_for(hash);
        let Ok(mut file) = std::fs::File::create(&path) else { return };

        let _ = writeln!(file, "{RENDER_HEADER}");
        write_slot_map(&mut file, slot_map);
        let _ = writeln!(file, "{VS_SEPARATOR}");
        let _ = write!(file, "{vs_msl}");
        // Ensure newline before FS separator
        if !vs_msl.ends_with('\n') {
            let _ = writeln!(file);
        }
        let _ = writeln!(file, "{FS_SEPARATOR}");
        let _ = write!(file, "{fs_msl}");
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
                self.hits, total, self.misses,
            );
        }
    }
}

// ─── SlotMap serialization ───────────────────────────────────────────

fn write_slot_map(file: &mut std::fs::File, slot_map: &SlotMap) {
    // Collect all valid slots
    let entries: Vec<_> = (0..=SIZES_BUFFER_BINDING)
        .filter_map(|b| slot_map.get(b).map(|s| (b, s)))
        .collect();
    let _ = writeln!(file, "{}", entries.len());
    for (binding, slot) in entries {
        let kind_char = match slot.kind {
            SlotKind::Buffer => 'B',
            SlotKind::Texture => 'T',
            SlotKind::Sampler => 'S',
        };
        let _ = writeln!(file, "{binding} {kind_char} {}", slot.metal_index);
    }
}

fn read_slot_map(lines: &mut impl Iterator<Item = Result<String, std::io::Error>>) -> Option<SlotMap> {
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
