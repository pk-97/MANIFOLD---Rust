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
    pub fn load_or_create(
        device: &ProtocolObject<dyn MTLDevice>,
        path: &Path,
    ) -> Option<Self> {
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
                unsafe { device.newBinaryArchiveWithDescriptor_error(&empty_desc) }
                    .unwrap_or_else(|e| {
                        panic!(
                            "Failed to create empty binary archive: {}",
                            e.localizedDescription()
                        )
                    })
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
        let url = NSURL::initWithString(NSURL::alloc(), &url_ns)
            .unwrap_or_else(|| panic!("NSURL initWithString: returned nil for {}", self.save_url_string));
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

/// Compute a stable hash for a render pipeline's identity.
pub fn render_pipeline_hash(wgsl_source: &str, vs_entry: &str, fs_entry: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    wgsl_source.hash(&mut hasher);
    vs_entry.hash(&mut hasher);
    fs_entry.hash(&mut hasher);
    hasher.finish()
}
