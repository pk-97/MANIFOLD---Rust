use std::path::{Path, PathBuf};

/// Info about a single axis directory of pre-sliced TIFFs.
pub struct AxisSlices {
    pub paths: Vec<PathBuf>,
    pub slice_count: u32,
}

/// Info about a scan (collection of per-axis slice directories).
pub struct ScanInfo {
    pub name: String,
    /// [axial, sagittal, coronal]
    pub axes: [Option<AxisSlices>; 3],
}

const AXIS_DIRS: [&str; 3] = ["axial", "sagittal", "coronal"];

impl ScanInfo {
    /// Check if a directory contains axis subdirectories with TIFF slices.
    pub fn discover(dir: &Path) -> Option<Self> {
        if !dir.is_dir() {
            return None;
        }

        let mut axes: [Option<AxisSlices>; 3] = [None, None, None];
        let mut has_any = false;

        for (i, axis_name) in AXIS_DIRS.iter().enumerate() {
            let axis_dir = dir.join(axis_name);
            if axis_dir.is_dir() {
                let mut paths: Vec<PathBuf> = std::fs::read_dir(&axis_dir)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension()
                            .is_some_and(|ext| ext == "tiff" || ext == "tif")
                    })
                    .collect();
                paths.sort();

                if !paths.is_empty() {
                    let count = paths.len() as u32;
                    axes[i] = Some(AxisSlices {
                        paths,
                        slice_count: count,
                    });
                    has_any = true;
                }
            }
        }

        if !has_any {
            return None;
        }

        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        Some(ScanInfo { name, axes })
    }
}

/// Discover all scan directories under the given base path.
pub fn discover_scans(base: &Path) -> Vec<ScanInfo> {
    if !base.is_dir() {
        return Vec::new();
    }
    let mut scans: Vec<ScanInfo> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| ScanInfo::discover(&p))
        .collect();
    scans.sort_by(|a, b| a.name.cmp(&b.name));
    scans
}

/// Load a single TIFF slice as 8-bit grayscale data.
/// Returns (width, height, data).
pub fn load_tiff_slice(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;

    let mut decoder = tiff::decoder::Decoder::new(file)
        .map_err(|e| format!("TIFF decode error {}: {}", path.display(), e))?;

    let (width, height) = decoder
        .dimensions()
        .map_err(|e| format!("TIFF dimensions error: {e}"))?;

    let image = decoder
        .read_image()
        .map_err(|e| format!("TIFF read error: {e}"))?;

    let data = match image {
        tiff::decoder::DecodingResult::U8(d) => d,
        tiff::decoder::DecodingResult::U16(d) => {
            d.iter().map(|&v| (v >> 8) as u8).collect()
        }
        tiff::decoder::DecodingResult::F32(d) => d
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0) as u8)
            .collect(),
        _ => return Err("Unsupported TIFF pixel format".into()),
    };

    Ok((width, height, data))
}
