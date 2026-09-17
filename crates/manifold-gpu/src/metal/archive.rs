//! MTLBinaryArchive — caches compiled Metal pipeline binaries to disk.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSString, NSURL};
use objc2_metal::{MTLBinaryArchive, MTLBinaryArchiveDescriptor, MTLDevice};

/// Pipeline binary archive — wraps MTLBinaryArchive.
pub struct GpuPipelineArchive {
    archive: Retained<ProtocolObject<dyn MTLBinaryArchive>>,
    /// Tracks which pipeline hashes have been added to the archive this session.
    added_hashes: std::collections::HashSet<u64>,
    /// Whether the archive was modified (new pipelines added).
    dirty: bool,
    /// File URL string for serialization.
    save_url_string: String,
}

// Safety: BinaryArchive is a Metal object — thread-safe per Metal's guarantees.
unsafe impl Send for GpuPipelineArchive {}
unsafe impl Sync for GpuPipelineArchive {}

impl GpuPipelineArchive {
    /// Load an existing archive from disk, or create a new empty one.
    pub fn load_or_create(device: &ProtocolObject<dyn MTLDevice>, path: &Path) -> Option<Self> {
        let url_string = format!("file://{}", path.display());
        let url_ns = NSString::from_str(&url_string);
        let url = NSURL::initWithString(NSURL::alloc(), &url_ns)
            .unwrap_or_else(|| panic!("NSURL initWithString: returned nil for {url_string}"));

        // Try loading existing archive
        let desc = unsafe { MTLBinaryArchiveDescriptor::init(MTLBinaryArchiveDescriptor::alloc()) };
        unsafe {
            desc.setUrl(Some(&url));
        }
        let archive = match unsafe { device.newBinaryArchiveWithDescriptor_error(&desc) } {
            Ok(archive) => {
                log::info!("Loaded pipeline archive from {}", path.display());
                archive
            }
            Err(_) => {
                // No existing archive or corrupt — create empty
                let empty_desc = unsafe {
                    MTLBinaryArchiveDescriptor::init(MTLBinaryArchiveDescriptor::alloc())
                };
                unsafe { device.newBinaryArchiveWithDescriptor_error(&empty_desc) }.unwrap_or_else(
                    |e| {
                        panic!(
                            "Failed to create empty binary archive: {}",
                            e.localizedDescription()
                        )
                    },
                )
            }
        };

        Some(Self {
            archive,
            added_hashes: std::collections::HashSet::new(),
            dirty: false,
            save_url_string: url_string,
        })
    }

    /// Get a reference to the underlying MTLBinaryArchive for pipeline creation.
    pub fn raw_archive(&self) -> &Retained<ProtocolObject<dyn MTLBinaryArchive>> {
        &self.archive
    }

    /// Record that a pipeline with the given hash was added to the archive.
    pub fn mark_added(&mut self, hash: u64) {
        self.added_hashes.insert(hash);
        self.dirty = true;
    }

    /// Check if a pipeline hash was already added this session.
    pub fn was_added(&self, hash: u64) -> bool {
        self.added_hashes.contains(&hash)
    }

    /// Whether the archive was modified and needs saving.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Serialize the archive to disk (if loaded and modified).
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let url_ns = NSString::from_str(&self.save_url_string);
        let url = NSURL::initWithString(NSURL::alloc(), &url_ns).unwrap_or_else(|| {
            panic!(
                "NSURL initWithString: returned nil for {}",
                self.save_url_string
            )
        });
        match unsafe { self.archive.serializeToURL_error(&url) } {
            Ok(()) => {
                log::info!(
                    "Saved pipeline archive ({} pipelines)",
                    self.added_hashes.len()
                );
                self.dirty = false;
            }
            Err(e) => {
                log::warn!(
                    "Failed to save pipeline archive: {}",
                    e.localizedDescription()
                );
            }
        }
    }
}

/// Compute a stable hash for a compute pipeline's identity.
pub fn pipeline_hash(wgsl_source: &str, entry_point: &str, use_half: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    wgsl_source.hash(&mut hasher);
    entry_point.hash(&mut hasher);
    use_half.hash(&mut hasher);
    hasher.finish()
}

/// Shader translation identity, shared by pipelines with different attachment
/// and blend state. The point-size rewrite changes emitted MSL, so it belongs
/// here too. A new namespace leaves old, ambiguous disk-cache entries unused.
pub(crate) fn render_shader_hash(
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    point_size_location: Option<u32>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    "render-shader-v2".hash(&mut hasher);
    wgsl_source.hash(&mut hasher);
    vs_entry.hash(&mut hasher);
    fs_entry.hash(&mut hasher);
    point_size_location.hash(&mut hasher);
    hasher.finish()
}

/// All variable Metal render-pipeline descriptor state. Used for both the
/// in-memory PSO cache and binary-archive insertion bookkeeping. Labels and
/// draw-time state (depth comparison, culling, fill mode) are not PSO state.
/// Keep this complete when adding descriptor options to a pipeline factory.
#[derive(Hash)]
pub(crate) struct RenderPipelineKey<'a> {
    pub shader: u64,
    pub color_format: Option<crate::GpuTextureFormat>,
    pub depth_format: Option<crate::GpuTextureFormat>,
    pub blend: Option<crate::GpuBlendState>,
    pub sample_count: u32,
    pub alpha_to_coverage: bool,
    pub aux_color_formats: &'a [crate::GpuTextureFormat],
    pub vertex_layout: Option<&'a crate::GpuVertexLayout>,
}

impl RenderPipelineKey<'_> {
    pub fn hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        "render-pipeline-v2".hash(&mut hasher);
        Hash::hash(self, &mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GpuBlendFactor as Factor, GpuBlendOp as Op, GpuBlendState, GpuTextureFormat as Format};

    fn key() -> RenderPipelineKey<'static> {
        RenderPipelineKey {
            shader: render_shader_hash("shader", "vs", "fs", None),
            color_format: Some(Format::Rgba16Float),
            depth_format: None,
            blend: None,
            sample_count: 1,
            alpha_to_coverage: false,
            aux_color_formats: &[],
            vertex_layout: None,
        }
    }

    #[test]
    fn render_identity_separates_attachment_and_raster_variants() {
        let original = key().hash();
        let mut variants = Vec::new();
        let mut changed = key();
        changed.color_format = Some(Format::Rgba8Unorm);
        variants.push(changed.hash());
        changed.color_format = None;
        variants.push(changed.hash());
        let mut changed = key();
        changed.depth_format = Some(Format::Depth32Float);
        variants.push(changed.hash());
        let mut changed = key();
        changed.sample_count = 4;
        variants.push(changed.hash());
        let mut changed = key();
        changed.alpha_to_coverage = true;
        variants.push(changed.hash());
        let mut changed = key();
        changed.aux_color_formats = &[Format::R32Float, Format::Rgba16Float];
        variants.push(changed.hash());
        changed.aux_color_formats = &[Format::Rgba16Float, Format::R32Float];
        variants.push(changed.hash());
        variants.push(original);
        let count = variants.len();
        assert_eq!(variants.into_iter().collect::<std::collections::HashSet<_>>().len(), count);
    }

    #[test]
    fn render_identity_includes_every_blend_component() {
        let blend = GpuBlendState {
            src_factor: Factor::One, dst_factor: Factor::Zero, operation: Op::Add,
            src_alpha_factor: Factor::One, dst_alpha_factor: Factor::Zero, alpha_operation: Op::Add,
        };
        let mut variants = vec![key().hash()];
        for b in [
            blend,
            GpuBlendState { src_factor: Factor::SrcAlpha, ..blend },
            GpuBlendState { dst_factor: Factor::One, ..blend },
            GpuBlendState { operation: Op::Max, ..blend },
            GpuBlendState { src_alpha_factor: Factor::SrcAlpha, ..blend },
            GpuBlendState { dst_alpha_factor: Factor::One, ..blend },
            GpuBlendState { alpha_operation: Op::Max, ..blend },
        ] {
            let mut changed = key();
            changed.blend = Some(b);
            variants.push(changed.hash());
        }
        let count = variants.len();
        assert_eq!(variants.into_iter().collect::<std::collections::HashSet<_>>().len(), count);
    }

    #[test]
    fn render_identity_includes_complete_vertex_layout() {
        use crate::{GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout};
        let attr = GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 0, shader_location: 0 };
        let mut variants = vec![key().hash()];
        for (stride, attribute) in [
            (16, attr), (24, attr),
            (16, GpuVertexAttribute { offset: 8, ..attr }),
            (16, GpuVertexAttribute { shader_location: 1, ..attr }),
            (16, GpuVertexAttribute { format: GpuVertexFormat::Float32x3, ..attr }),
        ] {
            let layout = GpuVertexLayout { stride, attributes: vec![attribute] };
            let mut changed = key();
            changed.vertex_layout = Some(&layout);
            variants.push(changed.hash());
        }
        let count = variants.len();
        assert_eq!(variants.into_iter().collect::<std::collections::HashSet<_>>().len(), count);
    }

    #[test]
    fn render_shader_identity_separates_point_size_rewrites_and_entry_points() {
        let hashes = [
            render_shader_hash("shader", "vs", "fs", None),
            render_shader_hash("shader", "vs", "fs", Some(0)),
            render_shader_hash("shader", "vs", "fs", Some(1)),
            render_shader_hash("shader", "other_vs", "fs", None),
            render_shader_hash("shader", "vs", "other_fs", None),
            render_shader_hash("other_shader", "vs", "fs", None),
        ];
        assert_eq!(hashes.into_iter().collect::<std::collections::HashSet<_>>().len(), hashes.len());
    }
}
