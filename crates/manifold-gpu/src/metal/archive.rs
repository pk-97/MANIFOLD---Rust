//! MTLBinaryArchive — caches compiled Metal pipeline binaries to disk.
//!
//! First launch: compiles shaders normally, adds each compiled pipeline to the
//! archive, serializes to disk.
//! Subsequent launches: loads pre-compiled GPU binaries — zero compilation latency.
//!
//! Cache invalidation: pipelines are keyed by a hash of the WGSL source +
//! entry point. If a shader changes between app versions, the stale entry
//! is recompiled and the archive is updated.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Create a properly retained NSURL from a string.
///
/// `metal::URL::new_with_string` uses `[NSURL URLWithString:]` which returns
/// an autoreleased object. The `metal` crate's wrapper assumes +1 ownership
/// but doesn't retain, so the autorelease pool drain sends a second release
/// → use-after-free. This helper creates the URL via `alloc` + `initWithString:`
/// which returns a +1 retained object (no autorelease), matching what the
/// `metal::URL` Drop expects.
fn create_retained_url(string: &str) -> metal::URL {
    use metal::foreign_types::ForeignType;
    use std::ffi::c_void;
    const UTF8_ENCODING: usize = 4;
    unsafe {
        // Create an NSString (autoreleased, but we don't store it)
        let ns_cls = objc::class!(NSString);
        let bytes = string.as_ptr().cast::<c_void>();
        let ns_str: *mut objc::runtime::Object = objc::msg_send![ns_cls, alloc];
        let ns_str: *mut objc::runtime::Object = objc::msg_send![
            ns_str,
            initWithBytes:bytes
            length:string.len()
            encoding:UTF8_ENCODING
        ];

        // Create NSURL via alloc+init (returns +1 retained, no autorelease)
        let url_cls = objc::class!(NSURL);
        let alloc: *mut objc::runtime::Object = objc::msg_send![url_cls, alloc];
        let obj: *mut objc::runtime::Object = objc::msg_send![alloc, initWithString: ns_str];

        // Release the NSString — NSURL retains it internally if needed
        let _: () = objc::msg_send![ns_str, release];

        assert!(
            !obj.is_null(),
            "NSURL initWithString: returned nil for {string}"
        );
        metal::URL::from_ptr(obj as *mut _)
    }
}

/// Pipeline binary archive — wraps MTLBinaryArchive.
/// Created once at startup, used for all pipeline creation, saved on shutdown.
pub struct GpuPipelineArchive {
    archive: metal::BinaryArchive,
    /// Tracks which pipeline hashes have been added to the archive this session.
    /// Used to avoid redundant addComputePipelineFunctions calls.
    added_hashes: std::collections::HashSet<u64>,
    /// Whether the archive was modified (new pipelines added).
    dirty: bool,
    /// File URL string for serialization. Stored as a String rather than
    /// metal::URL because URL wraps an autoreleased ObjC object whose
    /// lifetime is not visible to the Rust compiler — storing it long-term
    /// leads to use-after-free when an autorelease pool drains.
    save_url_string: String,
}

// Safety: BinaryArchive is a Metal object — thread-safe per Metal's guarantees.
unsafe impl Send for GpuPipelineArchive {}
unsafe impl Sync for GpuPipelineArchive {}

impl GpuPipelineArchive {
    /// Load an existing archive from disk, or create a new empty one.
    /// Binary archives require macOS 11+ / Apple Silicon (all supported targets).
    pub fn load_or_create(device: &metal::DeviceRef, path: &Path) -> Option<Self> {
        let url_string = format!("file://{}", path.display());
        let url = create_retained_url(&url_string);

        // Try loading existing archive
        let desc = metal::BinaryArchiveDescriptor::new();
        desc.set_url(&url);
        let archive = match device.new_binary_archive_with_descriptor(&desc) {
            Ok(archive) => {
                log::info!("Loaded pipeline archive from {}", path.display());
                archive
            }
            Err(_) => {
                // No existing archive or corrupt — create empty
                let empty_desc = metal::BinaryArchiveDescriptor::new();
                device
                    .new_binary_archive_with_descriptor(&empty_desc)
                    .unwrap_or_else(|e| panic!("Failed to create empty binary archive: {e}"))
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
    pub fn raw_archive(&self) -> &metal::BinaryArchiveRef {
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

    /// Serialize the archive to disk. Call after all pipelines have been created
    /// (e.g. at the end of startup or on shutdown).
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let url = create_retained_url(&self.save_url_string);
        match self.archive.serialize_to_url(&url) {
            Ok(_) => {
                log::info!(
                    "Saved pipeline archive ({} pipelines)",
                    self.added_hashes.len()
                );
                self.dirty = false;
            }
            Err(e) => {
                log::warn!("Failed to save pipeline archive: {e}");
            }
        }
    }
}

/// Compute a stable hash for a compute pipeline's identity
/// (WGSL source + entry point + half-precision flag).
/// Used for cache invalidation — if the hash changes, the pipeline is recompiled.
pub fn pipeline_hash(wgsl_source: &str, entry_point: &str, use_half: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    wgsl_source.hash(&mut hasher);
    entry_point.hash(&mut hasher);
    use_half.hash(&mut hasher);
    hasher.finish()
}

/// Compute a stable hash for a render pipeline's identity (WGSL source + VS/FS entry points).
pub fn render_pipeline_hash(wgsl_source: &str, vs_entry: &str, fs_entry: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    wgsl_source.hash(&mut hasher);
    vs_entry.hash(&mut hasher);
    fs_entry.hash(&mut hasher);
    hasher.finish()
}
