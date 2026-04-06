// Black Hole deflection map cache.
//
// Single-file binary format storing pre-baked Kerr geodesic deflection maps
// across a 2D grid of (cam_dist, tilt) values. At runtime, 4 grid neighbors
// are loaded, blended on the GPU, and consumed by the display shader —
// eliminating the per-frame geodesic compute cost.
//
// File layout:
//   Header (fixed-size): magic, version, grid dims, tex dims, spin, steps
//   cam_dist values: [f32; grid_rows]
//   tilt values: [f32; grid_cols]
//   offset table: [(u64 offset, u32 compressed_size); entries]
//   LZ4-compressed entry blocks (3 textures concatenated per entry)

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Arc;

pub const BH_CACHE_MAGIC: [u8; 4] = *b"BHCA";
pub const BH_CACHE_VERSION: u32 = 1;

/// Header describing a baked black hole cache file.
#[derive(Debug, Clone)]
pub struct BhCacheHeader {
    pub grid_rows: u32, // number of cam_dist samples
    pub grid_cols: u32, // number of tilt samples
    pub tex_width: u32,
    pub tex_height: u32,
    pub tex_count: u32, // always 3 (deflection1, deflection2, sky_dir)
    pub spin: f32,
    pub steps: f32,
    pub cam_dist_values: Vec<f32>,
    pub tilt_values: Vec<f32>,
}

impl BhCacheHeader {
    pub fn entry_count(&self) -> usize {
        (self.grid_rows * self.grid_cols) as usize
    }

    /// Bytes per fully-decompressed entry: 3 textures × W × H × 8 bytes (RGBA16Float).
    pub fn entry_bytes(&self) -> usize {
        self.tex_count as usize * self.tex_width as usize * self.tex_height as usize * 8
    }

    /// Bytes per single texture (RGBA16Float, tightly packed).
    pub fn texture_bytes(&self) -> usize {
        self.tex_width as usize * self.tex_height as usize * 8
    }

    pub fn grid_index(&self, dist_idx: usize, tilt_idx: usize) -> usize {
        dist_idx * self.grid_cols as usize + tilt_idx
    }
}

/// Logarithmic spacing of cam_dist values from 1.5 to 50.0.
/// Dense near the horizon where lensing changes rapidly.
pub fn grid_cam_dist_values(n: u32) -> Vec<f32> {
    assert!(n >= 2, "grid must have at least 2 cam_dist samples");
    let min = 1.5_f32;
    let max = 50.0_f32;
    let log_min = min.ln();
    let log_max = max.ln();
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            (log_min + (log_max - log_min) * t).exp()
        })
        .collect()
}

/// Cosine-spaced tilt values from 0 to 90 degrees.
/// Slightly denser near 0 and 90 where the visual changes most.
pub fn grid_tilt_values(n: u32) -> Vec<f32> {
    assert!(n >= 2, "grid must have at least 2 tilt samples");
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            // Pure linear is fine — Kerr deflection is reasonably smooth in tilt.
            // Cosine warp would push samples to edges; linear gives even coverage.
            90.0 * t
        })
        .collect()
}

/// 4 grid neighbors and bilinear blend weights for a query point.
#[derive(Debug, Clone, Copy)]
pub struct GridNeighbors {
    /// [top-left, top-right, bottom-left, bottom-right] linear indices
    /// where "top" means lower cam_dist and "left" means lower tilt.
    pub indices: [usize; 4],
    /// Fractional position within the cell: (frac_dist, frac_tilt) in [0, 1].
    pub frac: (f32, f32),
    /// True if the query tilt was mirrored from > 90 degrees.
    pub tilt_mirrored: bool,
}

impl GridNeighbors {
    pub fn weights(&self) -> [f32; 4] {
        let (fd, ft) = self.frac;
        [
            (1.0 - fd) * (1.0 - ft),
            (1.0 - fd) * ft,
            fd * (1.0 - ft),
            fd * ft,
        ]
    }
}

/// Find the 4 grid neighbors and blend weights for a query (cam_dist, tilt_deg).
/// Tilts > 90° are mirrored (Kerr is symmetric across the equatorial plane).
pub fn find_neighbors(header: &BhCacheHeader, cam_dist: f32, tilt_deg: f32) -> GridNeighbors {
    // Mirror tilt to 0-90 range.
    let mut tilt = tilt_deg.rem_euclid(360.0);
    let mut mirrored = false;
    if tilt > 180.0 {
        tilt = 360.0 - tilt;
    }
    if tilt > 90.0 {
        tilt = 180.0 - tilt;
        mirrored = true;
    }

    let (d_lo, d_hi, frac_d) =
        bracket_value(&header.cam_dist_values, cam_dist.clamp(1.5, 50.0));
    let (t_lo, t_hi, frac_t) = bracket_value(&header.tilt_values, tilt.clamp(0.0, 90.0));

    let cols = header.grid_cols as usize;
    let idx = |d: usize, t: usize| d * cols + t;

    GridNeighbors {
        indices: [
            idx(d_lo, t_lo),
            idx(d_lo, t_hi),
            idx(d_hi, t_lo),
            idx(d_hi, t_hi),
        ],
        frac: (frac_d, frac_t),
        tilt_mirrored: mirrored,
    }
}

/// Find the bracketing indices and fractional position for a value in a sorted array.
/// Returns (lo, hi, frac) where frac is 0 when v exactly matches a sample point.
fn bracket_value(values: &[f32], v: f32) -> (usize, usize, f32) {
    if v <= values[0] {
        return (0, 0, 0.0);
    }
    let last = values.len() - 1;
    if v >= values[last] {
        return (last, last, 0.0);
    }
    // Find the largest i such that values[i] <= v.
    for i in (0..last).rev() {
        if values[i] <= v {
            if (v - values[i]).abs() < f32::EPSILON {
                return (i, i, 0.0);
            }
            let span = values[i + 1] - values[i];
            let frac = if span > 0.0 { (v - values[i]) / span } else { 0.0 };
            return (i, i + 1, frac);
        }
    }
    (0, 0, 0.0)
}

// ─── File I/O ───────────────────────────────────────────────────────────────

/// Write a header to a writer in the binary format.
fn write_header<W: Write>(w: &mut W, h: &BhCacheHeader) -> std::io::Result<()> {
    w.write_all(&BH_CACHE_MAGIC)?;
    w.write_all(&BH_CACHE_VERSION.to_le_bytes())?;
    w.write_all(&h.grid_rows.to_le_bytes())?;
    w.write_all(&h.grid_cols.to_le_bytes())?;
    w.write_all(&h.tex_width.to_le_bytes())?;
    w.write_all(&h.tex_height.to_le_bytes())?;
    w.write_all(&h.tex_count.to_le_bytes())?;
    w.write_all(&h.spin.to_le_bytes())?;
    w.write_all(&h.steps.to_le_bytes())?;
    // 4 bytes reserved
    w.write_all(&0u32.to_le_bytes())?;
    for v in &h.cam_dist_values {
        w.write_all(&v.to_le_bytes())?;
    }
    for v in &h.tilt_values {
        w.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

fn read_header<R: Read>(r: &mut R) -> std::io::Result<BhCacheHeader> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if magic != BH_CACHE_MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad bhcache magic",
        ));
    }
    let version = read_u32(r)?;
    if version != BH_CACHE_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bhcache version {version} not supported (expected {BH_CACHE_VERSION})"),
        ));
    }
    let grid_rows = read_u32(r)?;
    let grid_cols = read_u32(r)?;
    let tex_width = read_u32(r)?;
    let tex_height = read_u32(r)?;
    let tex_count = read_u32(r)?;
    let spin = read_f32(r)?;
    let steps = read_f32(r)?;
    let _reserved = read_u32(r)?;
    let mut cam_dist_values = Vec::with_capacity(grid_rows as usize);
    for _ in 0..grid_rows {
        cam_dist_values.push(read_f32(r)?);
    }
    let mut tilt_values = Vec::with_capacity(grid_cols as usize);
    for _ in 0..grid_cols {
        tilt_values.push(read_f32(r)?);
    }
    Ok(BhCacheHeader {
        grid_rows,
        grid_cols,
        tex_width,
        tex_height,
        tex_count,
        spin,
        steps,
        cam_dist_values,
        tilt_values,
    })
}

fn read_u32<R: Read>(r: &mut R) -> std::io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(r: &mut R) -> std::io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_f32<R: Read>(r: &mut R) -> std::io::Result<f32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(f32::from_le_bytes(buf))
}

/// Streaming writer that builds a `.bhcache` file entry-by-entry.
pub struct BhCacheWriter {
    file: File,
    header: BhCacheHeader,
    offset_table_pos: u64,
    offset_table: Vec<(u64, u32)>,
    next_index: usize,
}

impl BhCacheWriter {
    pub fn create<P: AsRef<Path>>(path: P, header: BhCacheHeader) -> std::io::Result<Self> {
        let mut file = File::create(path)?;
        write_header(&mut file, &header)?;

        let entries = header.entry_count();
        let offset_table_pos = file.stream_position()?;
        // Reserve space for offset table: entries × (u64 offset + u32 size) = 12 bytes
        let placeholder = vec![0u8; entries * 12];
        file.write_all(&placeholder)?;

        Ok(Self {
            file,
            header,
            offset_table_pos,
            offset_table: Vec::with_capacity(entries),
            next_index: 0,
        })
    }

    /// Append a single entry. `raw` must be `header.entry_bytes()` bytes of
    /// concatenated RGBA16Float texture data (3 textures, tightly packed).
    pub fn write_entry(&mut self, raw: &[u8]) -> std::io::Result<()> {
        assert_eq!(
            raw.len(),
            self.header.entry_bytes(),
            "entry size mismatch"
        );
        let compressed = lz4_flex::compress_prepend_size(raw);
        let offset = self.file.seek(SeekFrom::End(0))?;
        let size = compressed.len() as u32;
        self.file.write_all(&compressed)?;
        self.offset_table.push((offset, size));
        self.next_index += 1;
        Ok(())
    }

    /// Finalize the file by writing the offset table back at its reserved position.
    pub fn finish(mut self) -> std::io::Result<()> {
        let expected = self.header.entry_count();
        assert_eq!(
            self.offset_table.len(),
            expected,
            "expected {} entries, wrote {}",
            expected,
            self.offset_table.len()
        );
        self.file.seek(SeekFrom::Start(self.offset_table_pos))?;
        for (offset, size) in &self.offset_table {
            self.file.write_all(&offset.to_le_bytes())?;
            self.file.write_all(&size.to_le_bytes())?;
        }
        self.file.flush()?;
        Ok(())
    }
}

/// Random-access reader for a `.bhcache` file. Header and offset table are
/// loaded eagerly; entries are decompressed on demand.
///
/// Reads use positioned `pread(2)` (`read_at`) so multiple threads can share
/// a single file descriptor without locking and without corrupting each other's
/// seek positions. Cloning is therefore free — `Arc<File>` increments a refcount.
#[derive(Clone)]
pub struct BhCacheReader {
    file: Arc<File>,
    header: BhCacheHeader,
    offset_table: Arc<Vec<(u64, u32)>>,
    expected_entry_bytes: usize,
}

impl BhCacheReader {
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let header = read_header(&mut file)?;
        let entries = header.entry_count();
        let mut offset_table = Vec::with_capacity(entries);
        for _ in 0..entries {
            let offset = read_u64(&mut file)?;
            let size = read_u32(&mut file)?;
            offset_table.push((offset, size));
        }
        let expected_entry_bytes = header.entry_bytes();
        Ok(Self {
            file: Arc::new(file),
            header,
            offset_table: Arc::new(offset_table),
            expected_entry_bytes,
        })
    }

    pub fn header(&self) -> &BhCacheHeader {
        &self.header
    }

    /// Read and decompress entry `index`. Thread-safe via positioned reads —
    /// does not mutate any shared seek state.
    pub fn read_entry(&self, index: usize) -> std::io::Result<Vec<u8>> {
        let (offset, size) = self.offset_table[index];
        let mut compressed = vec![0u8; size as usize];
        // pread loop — partial reads are possible on very large files.
        let mut filled = 0usize;
        while filled < compressed.len() {
            let n = self
                .file
                .read_at(&mut compressed[filled..], offset + filled as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "short read at offset {} (got {} of {})",
                        offset,
                        filled,
                        compressed.len()
                    ),
                ));
            }
            filled += n;
        }
        let decompressed = lz4_flex::decompress_size_prepended(&compressed)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if decompressed.len() != self.expected_entry_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "entry {} decompressed to {} bytes, expected {}",
                    index,
                    decompressed.len(),
                    self.expected_entry_bytes,
                ),
            ));
        }
        Ok(decompressed)
    }

    /// Cheap clone — shares the underlying fd and offset table via Arc.
    pub fn try_clone_for_thread(&self) -> std::io::Result<BhCacheReader> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cam_dist_logarithmic() {
        let v = grid_cam_dist_values(10);
        assert_eq!(v.len(), 10);
        assert!((v[0] - 1.5).abs() < 0.001);
        assert!((v[9] - 50.0).abs() < 0.001);
        for w in v.windows(2) {
            assert!(w[1] > w[0]);
        }
        // Logarithmic: ratios should be roughly constant.
        let ratio_lo = v[1] / v[0];
        let ratio_hi = v[9] / v[8];
        assert!((ratio_lo - ratio_hi).abs() < 0.01);
    }

    #[test]
    fn tilt_covers_range() {
        let v = grid_tilt_values(10);
        assert_eq!(v.len(), 10);
        assert!(v[0].abs() < 0.001);
        assert!((v[9] - 90.0).abs() < 0.001);
        for w in v.windows(2) {
            assert!(w[1] > w[0]);
        }
    }

    fn make_header() -> BhCacheHeader {
        BhCacheHeader {
            grid_rows: 4,
            grid_cols: 4,
            tex_width: 8,
            tex_height: 8,
            tex_count: 3,
            spin: 0.5,
            steps: 100.0,
            cam_dist_values: grid_cam_dist_values(4),
            tilt_values: grid_tilt_values(4),
        }
    }

    #[test]
    fn neighbors_at_grid_point() {
        let h = make_header();
        let n = find_neighbors(&h, h.cam_dist_values[1], h.tilt_values[2]);
        // At an exact grid point, frac should be 0 and indices should pin.
        assert_eq!(n.frac.0, 0.0);
        assert_eq!(n.frac.1, 0.0);
        // The TL index should be the exact (1, 2) grid point.
        assert_eq!(n.indices[0], 1 * 4 + 2);
    }

    #[test]
    fn neighbors_between_points() {
        let h = make_header();
        let mid_dist = (h.cam_dist_values[1] + h.cam_dist_values[2]) * 0.5;
        let mid_tilt = (h.tilt_values[1] + h.tilt_values[2]) * 0.5;
        let n = find_neighbors(&h, mid_dist, mid_tilt);
        assert!((n.frac.0 - 0.5).abs() < 0.01);
        assert!((n.frac.1 - 0.5).abs() < 0.01);
        let weights = n.weights();
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 0.001);
    }

    #[test]
    fn neighbors_tilt_mirroring() {
        let h = make_header();
        let n = find_neighbors(&h, 10.0, 135.0);
        assert!(n.tilt_mirrored);
        // 135° mirrors to 45° → should match a query at tilt=45°
        let m = find_neighbors(&h, 10.0, 45.0);
        assert_eq!(n.indices, m.indices);
        assert_eq!(n.frac, m.frac);
    }

    #[test]
    fn neighbors_tilt_below_zero_mirrors() {
        let h = make_header();
        // -10° → 350° rem_euclid → 360-350 = 10° mirror once → 10°
        let n = find_neighbors(&h, 10.0, -10.0);
        let m = find_neighbors(&h, 10.0, 10.0);
        assert_eq!(n.indices, m.indices);
    }

    #[test]
    fn neighbors_clamped_to_range() {
        let h = make_header();
        let lo = find_neighbors(&h, 0.5, 30.0);
        let hi = find_neighbors(&h, 200.0, 30.0);
        // Clamping must not panic and must return valid indices.
        for idx in lo.indices.iter().chain(hi.indices.iter()) {
            assert!(*idx < h.entry_count());
        }
    }

    #[test]
    fn lz4_round_trip() {
        let mut data = vec![0u8; 1024];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let compressed = lz4_flex::compress_prepend_size(&data);
        let decompressed = lz4_flex::decompress_size_prepended(&compressed).unwrap();
        assert_eq!(data, decompressed);
    }

    #[test]
    fn concurrent_reads_do_not_interfere() {
        // Build a small cache, then hammer it from 8 threads in parallel.
        // Without positioned reads (read_at), shared seek state on the same fd
        // would cause threads to read each other's bytes — this test would fail.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("bhcache_concurrent_{}.bhcache", std::process::id()));

        let header = BhCacheHeader {
            grid_rows: 4,
            grid_cols: 4,
            tex_width: 32,
            tex_height: 32,
            tex_count: 3,
            spin: 0.0,
            steps: 50.0,
            cam_dist_values: vec![1.5, 5.0, 15.0, 50.0],
            tilt_values: vec![0.0, 30.0, 60.0, 90.0],
        };
        let entry_bytes = header.entry_bytes();
        let mut writer = BhCacheWriter::create(&path, header.clone()).unwrap();
        for i in 0..16 {
            let mut data = vec![0u8; entry_bytes];
            // Tag every byte with a per-entry pattern so we can detect cross-talk.
            for (j, b) in data.iter_mut().enumerate() {
                *b = ((i * 31 + j) % 251) as u8;
            }
            writer.write_entry(&data).unwrap();
        }
        writer.finish().unwrap();

        let reader = BhCacheReader::open(&path).unwrap();
        let mut handles = Vec::new();
        for thread_id in 0..8 {
            let r = reader.clone();
            handles.push(std::thread::spawn(move || {
                for round in 0..32 {
                    let i = (thread_id * 7 + round * 3) % 16;
                    let data = r.read_entry(i).expect("read_entry");
                    assert_eq!(data.len(), entry_bytes);
                    for (j, b) in data.iter().enumerate() {
                        assert_eq!(
                            *b as usize,
                            (i * 31 + j) % 251,
                            "thread {thread_id} round {round} entry {i} byte {j}",
                        );
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[ignore] // requires `manifold bake-black-hole` to have produced the file
    fn open_real_bake_file() {
        let path = std::path::Path::new("../../assets/black-hole.bhcache");
        if !path.exists() {
            eprintln!("skipping: no bake file");
            return;
        }
        let mut reader = BhCacheReader::open(path).expect("open bake file");
        let h = reader.header().clone();
        assert_eq!(h.tex_count, 3);
        assert!(h.grid_rows >= 2);
        assert!(h.grid_cols >= 2);
        assert!(h.tex_width >= 64);
        assert_eq!(h.tex_width, h.tex_height);
        // Read several entries to verify offset table is correct.
        for idx in [0, h.entry_count() / 2, h.entry_count() - 1] {
            let data = reader.read_entry(idx).expect("read entry");
            assert_eq!(data.len(), h.entry_bytes());
        }
    }

    #[test]
    fn cache_file_round_trip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("bhcache_test_{}.bhcache", std::process::id()));

        let header = BhCacheHeader {
            grid_rows: 2,
            grid_cols: 2,
            tex_width: 4,
            tex_height: 4,
            tex_count: 3,
            spin: 0.0,
            steps: 50.0,
            cam_dist_values: vec![1.5, 50.0],
            tilt_values: vec![0.0, 90.0],
        };

        let entry_bytes = header.entry_bytes();
        let mut writer = BhCacheWriter::create(&path, header.clone()).unwrap();
        for i in 0..4 {
            let mut data = vec![0u8; entry_bytes];
            // Tag each entry with its index in byte 0 so we can verify ordering.
            data[0] = i as u8;
            writer.write_entry(&data).unwrap();
        }
        writer.finish().unwrap();

        let mut reader = BhCacheReader::open(&path).unwrap();
        assert_eq!(reader.header().entry_count(), 4);
        assert_eq!(reader.header().tex_width, 4);
        assert_eq!(reader.header().spin, 0.0);
        for i in 0..4 {
            let data = reader.read_entry(i).unwrap();
            assert_eq!(data.len(), entry_bytes);
            assert_eq!(data[0], i as u8);
        }

        let _ = std::fs::remove_file(&path);
    }
}
