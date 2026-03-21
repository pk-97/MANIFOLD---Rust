use manifold_core::mri::{MriVolHeader, VoxelFormat, MRIVOL_MAGIC};
use std::path::Path;

/// Loaded MRI volume ready for GPU upload.
pub struct MriVolumeData {
    pub header: MriVolHeader,
    /// Voxel data normalized to f32 [0.0, 1.0] for all frames.
    /// Layout: frame-major, then Z, Y, X (row-major per frame).
    pub voxels_f32: Vec<f32>,
}

/// GPU-resident MRI volume (single frame uploaded as a 3D texture).
pub struct MriVolumeGpu {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub dim: [u32; 3],
    pub frames: u32,
    pub voxel_range: [f32; 2],
    pub spacing: [f32; 3],
    /// Auto-computed window: [low, high] percentile range in normalized [0,1] space.
    /// Window Center/Width params map relative to this range.
    pub auto_window: [f32; 2],
}

impl MriVolumeData {
    /// Parse a .mrivol file from disk.
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

        if data.len() < 12 {
            return Err("File too small for .mrivol header".into());
        }

        // Verify magic
        if &data[0..8] != MRIVOL_MAGIC {
            return Err("Invalid .mrivol magic bytes".into());
        }

        // Read header length
        let header_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let header_end = 12 + header_len;

        if data.len() < header_end {
            return Err(format!(
                "File truncated: need {} bytes for header, have {}",
                header_end,
                data.len()
            ));
        }

        // Parse JSON header
        let header_json = &data[12..header_end];
        let header: MriVolHeader = serde_json::from_slice(header_json)
            .map_err(|e| format!("Failed to parse .mrivol header: {}", e))?;

        // Validate data section
        let expected_data_size = header.data_byte_size();
        let actual_data_size = data.len() - header_end;
        if actual_data_size < expected_data_size {
            return Err(format!(
                "Data section too small: expected {} bytes, got {}",
                expected_data_size, actual_data_size
            ));
        }

        // Decode voxels to f32 [0.0, 1.0]
        let raw = &data[header_end..header_end + expected_data_size];
        let total_voxels = header.total_voxels();
        let mut voxels_f32 = Vec::with_capacity(total_voxels);

        match header.voxel_format {
            VoxelFormat::R8 => {
                for &byte in raw {
                    voxels_f32.push(byte as f32 / 255.0);
                }
            }
            VoxelFormat::R16 => {
                for chunk in raw.chunks_exact(2) {
                    let val = u16::from_le_bytes([chunk[0], chunk[1]]);
                    voxels_f32.push(val as f32 / 65535.0);
                }
            }
            VoxelFormat::R32Float => {
                for chunk in raw.chunks_exact(4) {
                    let val = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    voxels_f32.push(val);
                }
            }
        }

        log::info!(
            "Loaded .mrivol: {}x{}x{} x{} frames, range [{:.0}, {:.0}], {:.1} MB",
            header.dim_x, header.dim_y, header.dim_z, header.frames,
            header.voxel_range[0], header.voxel_range[1],
            (total_voxels * 4) as f64 / (1024.0 * 1024.0),
        );

        Ok(Self { header, voxels_f32 })
    }

    /// Compute auto-window range from percentiles of non-zero voxels.
    /// Returns [low, high] in normalized [0,1] space.
    pub fn compute_auto_window(&self) -> [f32; 2] {
        // Collect non-zero voxels (skip background/air)
        let threshold = 0.001; // skip near-zero voxels
        let mut vals: Vec<f32> = self.voxels_f32.iter()
            .copied()
            .filter(|&v| v > threshold)
            .collect();

        if vals.is_empty() {
            return [0.0, 1.0];
        }

        vals.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let n = vals.len();
        let p2 = vals[(n as f64 * 0.02) as usize];
        let p98 = vals[((n as f64 * 0.98) as usize).min(n - 1)];

        log::info!("  Auto-window: [{:.4}, {:.4}] (2nd-98th percentile of {} non-zero voxels)", p2, p98, n);

        [p2, p98]
    }
}

impl MriVolumeGpu {
    /// Upload a single frame (or the only frame) to a 3D texture.
    /// Uses Rgba16Float for Metal filterability (textureSample compatibility).
    pub fn from_volume_data(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vol: &MriVolumeData,
        frame_index: u32,
        auto_window: [f32; 2],
    ) -> Self {
        let h = &vol.header;
        let dim = [h.dim_x, h.dim_y, h.dim_z];
        let frame_voxels = h.frame_voxels();
        let frame_offset = frame_index as usize * frame_voxels;

        // Convert f32 → Rgba16Float (4x f16 per texel).
        // We store the intensity in all 4 channels for simplicity;
        // the shader reads .r only.
        //
        // The .mrivol data is stored in NIfTI C-order: for shape (X, Y, Z),
        // Z varies fastest in memory. But GPU texture upload expects X to vary
        // fastest (bytes_per_row covers one X scanline). So we rearrange:
        // source index [x + y*dim_x + z*dim_x*dim_y] (GPU order) maps to
        // voxels_f32 index [z + y*dim_z + x*dim_z*dim_y] (NIfTI C-order).
        //
        // Also: bytes_per_row must be a multiple of 256 for wgpu.
        let frame_data = &vol.voxels_f32[frame_offset..frame_offset + frame_voxels];
        let texel_bytes: u32 = 8; // Rgba16Float = 4 x f16
        let unpadded_bytes_per_row = dim[0] * texel_bytes;
        let padded_bytes_per_row = (unpadded_bytes_per_row + 255) & !255;
        let pad_bytes = (padded_bytes_per_row - unpadded_bytes_per_row) as usize;
        let total_size = padded_bytes_per_row as usize * dim[1] as usize * dim[2] as usize;
        let mut rgba16_data: Vec<u8> = Vec::with_capacity(total_size);

        let dx = dim[0] as usize;
        let dy = dim[1] as usize;
        let dz = dim[2] as usize;

        for z in 0..dz {
            for y in 0..dy {
                for x in 0..dx {
                    // NIfTI C-order index: shape is (dim_x, dim_y, dim_z),
                    // so index = x * (dim_y * dim_z) + y * dim_z + z
                    let src_idx = x * (dy * dz) + y * dz + z;
                    let val = frame_data[src_idx];
                    let h16 = half::f16::from_f32(val);
                    let bytes = h16.to_le_bytes();
                    rgba16_data.extend_from_slice(&bytes);
                    rgba16_data.extend_from_slice(&bytes);
                    rgba16_data.extend_from_slice(&bytes);
                    rgba16_data.extend_from_slice(&bytes);
                }
                // Pad row to 256-byte alignment
                rgba16_data.extend(std::iter::repeat_n(0u8, pad_bytes));
            }
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("MRI Volume 3D"),
            size: wgpu::Extent3d {
                width: dim[0],
                height: dim[1],
                depth_or_array_layers: dim[2],
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba16_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(dim[1]),
            },
            wgpu::Extent3d {
                width: dim[0],
                height: dim[1],
                depth_or_array_layers: dim[2],
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });

        Self {
            texture,
            view,
            dim,
            frames: h.frames,
            voxel_range: h.voxel_range,
            spacing: h.voxel_spacing,
            auto_window,
        }
    }
}
