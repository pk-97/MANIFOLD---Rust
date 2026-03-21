use serde::{Deserialize, Serialize};

/// Magic bytes at the start of every .mrivol file.
pub const MRIVOL_MAGIC: &[u8; 8] = b"MRIVOL\x01\x00";

/// Supported voxel data formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoxelFormat {
    /// 8-bit unsigned integer (0–255).
    R8,
    /// 16-bit unsigned integer, little-endian (0–65535).
    R16,
    /// 32-bit float, little-endian.
    R32Float,
}

impl VoxelFormat {
    /// Bytes per voxel for this format.
    pub fn bytes_per_voxel(&self) -> usize {
        match self {
            VoxelFormat::R8 => 1,
            VoxelFormat::R16 => 2,
            VoxelFormat::R32Float => 4,
        }
    }
}

/// Header for the .mrivol binary volume format.
///
/// File layout:
///   [8 bytes]  Magic: "MRIVOL\x01\x00"
///   [4 bytes]  Header length (u32 LE) — byte count of JSON header
///   [N bytes]  JSON header (UTF-8)
///   [M bytes]  Raw voxel data (row-major: X fastest, then Y, then Z, then T)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MriVolHeader {
    /// Format version (currently 1).
    pub version: u32,
    /// Volume width (X axis).
    pub dim_x: u32,
    /// Volume height (Y axis).
    pub dim_y: u32,
    /// Volume depth (Z axis / number of slices).
    pub dim_z: u32,
    /// Number of temporal frames (1 = static, >1 = cine).
    pub frames: u32,
    /// Voxel data format.
    pub voxel_format: VoxelFormat,
    /// Intensity range [min, max] in the dataset.
    pub voxel_range: [f32; 2],
    /// Physical voxel spacing in mm [x, y, z].
    pub voxel_spacing: [f32; 3],
    /// 3x3 orientation matrix (row-major), mapping voxel to patient coords.
    #[serde(default = "default_orientation")]
    pub orientation: [f32; 9],
    /// Optional human-readable description.
    #[serde(default)]
    pub description: String,
}

fn default_orientation() -> [f32; 9] {
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
}

impl MriVolHeader {
    /// Total number of voxels across all frames.
    pub fn total_voxels(&self) -> usize {
        self.dim_x as usize * self.dim_y as usize * self.dim_z as usize * self.frames as usize
    }

    /// Total byte size of the raw voxel data section.
    pub fn data_byte_size(&self) -> usize {
        self.total_voxels() * self.voxel_format.bytes_per_voxel()
    }

    /// Number of voxels in a single 3D frame.
    pub fn frame_voxels(&self) -> usize {
        self.dim_x as usize * self.dim_y as usize * self.dim_z as usize
    }
}
