//! FFI wrapper for the versioned BlobDetector V2 region API.

use crate::region_detector::{
    BlobRegionOptionsV2, BlobRegionV2, MAX_REGIONS, Region, RegionDetector, RegionError,
    RegionOptions, validate_region_input,
};
use libloading::Library;
use std::ffi::c_void;
use std::path::Path;

type FnCreate = unsafe extern "C" fn() -> *mut c_void;
type FnDestroy = unsafe extern "C" fn(*mut c_void);
type FnProcess = unsafe extern "C" fn(
    *mut c_void,
    *const u8,
    usize,
    u32,
    u32,
    *const BlobRegionOptionsV2,
    *mut u8,
    usize,
    *mut BlobRegionV2,
    usize,
) -> i32;
type FnProcessBounded = unsafe extern "C" fn(
    *mut c_void,
    *const u8,
    usize,
    u32,
    u32,
    *const BlobRegionOptionsV2,
    *mut u8,
    usize,
    *mut BlobRegionV2,
    usize,
    f32,
) -> i32;

/// FFI-backed V2 detector.  The plugin is intentionally leaked after symbol
/// resolution, matching the existing BlobDetector wrapper's non-unloading
/// policy for OpenCV's worker libraries.
pub struct FfiRegionDetector {
    fn_destroy: FnDestroy,
    fn_process: FnProcess,
    fn_process_bounded: FnProcessBounded,
    handle: *mut c_void,
}

// The native handle is exclusively accessed through &mut self in process.
unsafe impl Send for FfiRegionDetector {}

impl FfiRegionDetector {
    pub fn new() -> Result<Self, String> {
        let path = super::resolve_bundle_path("BlobDetector").ok_or_else(|| {
            "BlobDetector V2 bundle not found (set MANIFOLD_BLOBDETECTOR_PLUGIN to a rebuilt bundle)"
                .to_owned()
        })?;
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self, String> {
        let library = unsafe { Library::new(path) }.map_err(|error| {
            format!(
                "failed to load BlobDetector V2 bundle {}: {error}",
                path.display()
            )
        })?;

        let fn_create = unsafe {
            library
                .get::<FnCreate>(b"BlobDetectorV2_Create\0")
                .map_err(|error| missing_symbol(path, "BlobDetectorV2_Create", &error))
                .map(|symbol| *symbol)?
        };
        let fn_destroy = unsafe {
            library
                .get::<FnDestroy>(b"BlobDetectorV2_Destroy\0")
                .map_err(|error| missing_symbol(path, "BlobDetectorV2_Destroy", &error))
                .map(|symbol| *symbol)?
        };
        let fn_process = unsafe {
            library
                .get::<FnProcess>(b"BlobDetectorV2_Process\0")
                .map_err(|error| missing_symbol(path, "BlobDetectorV2_Process", &error))
                .map(|symbol| *symbol)?
        };
        let fn_process_bounded = unsafe {
            library
                .get::<FnProcessBounded>(b"BlobDetectorV2_ProcessBounded\0")
                .map_err(|error| missing_symbol(path, "BlobDetectorV2_ProcessBounded", &error))
                .map(|symbol| *symbol)?
        };

        let handle = unsafe { fn_create() };
        if handle.is_null() {
            return Err(format!(
                "BlobDetectorV2_Create returned null for {}",
                path.display()
            ));
        }

        // The plugin may own OpenCV/TBB worker threads.  Keep the library
        // mapped until process exit, as the legacy wrapper does.
        std::mem::forget(library);

        log::info!(
            "[FfiRegionDetector] Loaded BlobDetector V2 symbols from {}",
            path.display()
        );
        Ok(Self {
            fn_destroy,
            fn_process,
            fn_process_bounded,
            handle,
        })
    }
}

fn missing_symbol(path: &Path, symbol: &str, error: &dyn std::fmt::Display) -> String {
    format!("{}: {error}", missing_symbol_name(path, symbol))
}

fn missing_symbol_name(path: &Path, symbol: &str) -> String {
    format!(
        "BlobDetector V2 bundle {} is missing required symbol {symbol}",
        path.display()
    )
}

impl Drop for FfiRegionDetector {
    fn drop(&mut self) {
        unsafe { (self.fn_destroy)(self.handle) };
    }
}

impl RegionDetector for FfiRegionDetector {
    fn process(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        options: RegionOptions,
        labels: &mut [u8],
        regions: &mut [Region; MAX_REGIONS],
    ) -> Result<usize, RegionError> {
        self.process_with_bound(rgba, width, height, options, None, labels, regions)
    }

    fn process_bounded(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        options: RegionOptions,
        max_box_area: f32,
        labels: &mut [u8],
        regions: &mut [Region; MAX_REGIONS],
    ) -> Result<usize, RegionError> {
        self.process_with_bound(
            rgba,
            width,
            height,
            options,
            Some(max_box_area),
            labels,
            regions,
        )
    }
}

impl FfiRegionDetector {
    fn process_with_bound(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        options: RegionOptions,
        max_box_area: Option<f32>,
        labels: &mut [u8],
        regions: &mut [Region; MAX_REGIONS],
    ) -> Result<usize, RegionError> {
        // Always clear first so stale labels/records cannot escape a failed
        // call or a native exception translated to -2.
        labels.fill(0);
        regions.fill(Region::default());

        validate_region_input(rgba, width, height, options, labels, regions.len())?;
        if let Some(max_box_area) = max_box_area
            && (!max_box_area.is_finite() || !(0.0..=1.0).contains(&max_box_area))
        {
            return Err(RegionError::InvalidInput);
        }

        let result = unsafe {
            match max_box_area {
                Some(max_box_area) => (self.fn_process_bounded)(
                    self.handle,
                    rgba.as_ptr(),
                    rgba.len(),
                    width,
                    height,
                    &options,
                    labels.as_mut_ptr(),
                    labels.len(),
                    regions.as_mut_ptr(),
                    regions.len(),
                    max_box_area,
                ),
                None => (self.fn_process)(
                    self.handle,
                    rgba.as_ptr(),
                    rgba.len(),
                    width,
                    height,
                    &options,
                    labels.as_mut_ptr(),
                    labels.len(),
                    regions.as_mut_ptr(),
                    regions.len(),
                ),
            }
        };

        if (0..=MAX_REGIONS as i32).contains(&result) {
            return Ok(result as usize);
        }

        match result {
            -1 => {
                labels.fill(0);
                regions.fill(Region::default());
                Err(RegionError::InvalidInput)
            }
            -2 => {
                labels.fill(0);
                regions.fill(Region::default());
                Err(RegionError::NativeFailure)
            }
            _ => {
                labels.fill(0);
                regions.fill(Region::default());
                Err(RegionError::NativeFailure)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region_detector::RegionOptions;

    #[cfg(target_os = "macos")]
    #[test]
    fn blob_v2_missing_symbols_reported() {
        // A system library is a loadable stand-in for a prior bundle that
        // lacks V2 exports. Loading an embedded OpenCV dependency directly
        // would fail on its own @rpath before symbol resolution.
        let non_v2_library = Path::new("/usr/lib/libSystem.B.dylib");
        let message = match FfiRegionDetector::load_from_path(non_v2_library) {
            Ok(_) => panic!("OpenCV core unexpectedly exports BlobDetector V2"),
            Err(message) => message,
        };
        assert!(message.contains("BlobDetector V2"));
        assert!(message.contains("missing required symbol BlobDetectorV2_Create"));
        assert!(
            message.contains(&non_v2_library.display().to_string()),
            "{message}"
        );
    }

    #[cfg(target_os = "macos")]
    fn bundled_plugin() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/plugins/BlobDetector.bundle")
    }

    #[cfg(target_os = "macos")]
    fn copy_directory(source: &Path, destination: &Path) {
        std::fs::create_dir_all(destination).expect("create relocated bundle directory");
        for entry in std::fs::read_dir(source).expect("read bundled plugin") {
            let entry = entry.expect("read bundle entry");
            let target = destination.join(entry.file_name());
            if entry.file_type().expect("bundle entry type").is_dir() {
                copy_directory(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).expect("copy signed bundle file");
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn blob_v2_relocated_bundle_keeps_legacy_symbols() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("blob-v2-relocated-{}-{nonce}", std::process::id()));
        let bundle = root.join("BlobDetector.bundle");
        copy_directory(&bundled_plugin(), &bundle);
        let binary = bundle.join("Contents/MacOS/BlobDetector");

        let signature = std::process::Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&bundle)
            .output()
            .expect("run codesign");
        assert!(
            signature.status.success(),
            "{}",
            String::from_utf8_lossy(&signature.stderr)
        );
        let dependencies = std::process::Command::new("otool")
            .arg("-L")
            .arg(&binary)
            .output()
            .expect("run otool");
        assert!(dependencies.status.success());
        let dependency_text = String::from_utf8_lossy(&dependencies.stdout);
        assert!(
            !dependency_text.contains("/opt/homebrew/"),
            "{dependency_text}"
        );

        let library = unsafe { Library::new(&binary) }.expect("load relocated bundle");
        type LegacyCreate = unsafe extern "C" fn(i32) -> *mut c_void;
        type LegacyDestroy = unsafe extern "C" fn(*mut c_void);
        type LegacyProcess =
            unsafe extern "C" fn(*mut c_void, *const u8, i32, i32, f32, f32, *mut f32) -> i32;
        unsafe {
            let create = library
                .get::<LegacyCreate>(b"BlobDetector_Create\0")
                .expect("legacy create");
            let destroy = library
                .get::<LegacyDestroy>(b"BlobDetector_Destroy\0")
                .expect("legacy destroy");
            let process = library
                .get::<LegacyProcess>(b"BlobDetector_Process\0")
                .expect("legacy process");
            let handle = create(8);
            assert!(!handle.is_null());
            let rgba = [0u8; 4 * 4 * 4];
            let mut boxes = [0.0f32; 8 * 4];
            assert_eq!(
                process(handle, rgba.as_ptr(), 4, 4, 0.5, 0.5, boxes.as_mut_ptr()),
                0
            );
            destroy(handle);
        }
        std::mem::forget(library); // Legacy OpenCV/TBB non-unloading policy.

        let mut detector =
            FfiRegionDetector::load_from_path(&binary).expect("V2 relocated symbols");
        let mut labels = [0u8; 16];
        let mut regions = [Region::default(); MAX_REGIONS];
        let mut rgba = [0u8; 4 * 4 * 4];
        rgba[0] = 255;
        assert_eq!(
            detector.process(
                &rgba,
                4,
                4,
                RegionOptions {
                    threshold: 1.0,
                    min_area: 0.0,
                    max_area: 1.0,
                    min_aspect: 0.0,
                    max_aspect: 20.0,
                    max_regions: 32,
                },
                &mut labels,
                &mut regions
            ),
            Ok(1)
        );
        assert_eq!(labels[0], 1);
        assert_eq!(labels[1], 0);
        std::fs::remove_dir_all(root).expect("remove relocated test bundle");
    }

    #[test]
    fn blob_v2_outputs_are_cleared_before_validation() {
        // This exercises the public wrapper's validation path without
        // requiring a native bundle.  The fixture tests which need OpenCV
        // intentionally construct the wrapper and therefore fail loudly when
        // the rebuilt bundle is absent.
        unsafe extern "C" fn never_process(
            _handle: *mut c_void,
            _rgba: *const u8,
            _rgba_len: usize,
            _width: u32,
            _height: u32,
            _options: *const BlobRegionOptionsV2,
            _labels: *mut u8,
            _labels_len: usize,
            _regions: *mut BlobRegionV2,
            _regions_capacity: usize,
        ) -> i32 {
            panic!("invalid input reached native process");
        }
        unsafe extern "C" fn never_process_bounded(
            _handle: *mut c_void,
            _rgba: *const u8,
            _rgba_len: usize,
            _width: u32,
            _height: u32,
            _options: *const BlobRegionOptionsV2,
            _labels: *mut u8,
            _labels_len: usize,
            _regions: *mut BlobRegionV2,
            _regions_capacity: usize,
            _max_box_area: f32,
        ) -> i32 {
            panic!("invalid input reached native bounded process");
        }
        unsafe extern "C" fn noop_destroy(_handle: *mut c_void) {}

        let mut labels = vec![255; 4];
        let mut regions = [Region {
            label: 7,
            ..Region::default()
        }; MAX_REGIONS];
        let options = RegionOptions {
            threshold: 0.5,
            min_area: 0.0,
            max_area: 1.0,
            min_aspect: 0.05,
            max_aspect: 20.0,
            max_regions: 8,
        };
        let mut detector = FfiRegionDetector {
            fn_destroy: noop_destroy,
            fn_process: never_process,
            fn_process_bounded: never_process_bounded,
            handle: std::ptr::null_mut(),
        };
        assert_eq!(
            detector.process(&[0; 3], 1, 1, options, &mut labels, &mut regions),
            Err(RegionError::InvalidInput)
        );
        assert!(labels.iter().all(|value| *value == 0));
        assert!(regions.iter().all(|region| *region == Region::default()));
    }
}
