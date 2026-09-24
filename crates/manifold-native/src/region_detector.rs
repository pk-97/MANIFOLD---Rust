//! Versioned connected-component detector ABI used by Blob Track V2.
//!
//! The native plugin owns the OpenCV implementation.  This module owns the
//! fixed-size records and the Rust-side validation which protects that ABI.

pub const MAX_REGIONS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Region {
    pub label: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub area: f32,
    pub cx: f32,
    pub cy: f32,
}

/// Name used by the C header.  Keeping the alias here makes it harder for a
/// caller to accidentally introduce a second, subtly different ABI record.
pub type BlobRegionV2 = Region;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionOptions {
    pub threshold: f32,
    pub min_area: f32,
    pub max_area: f32,
    pub min_aspect: f32,
    pub max_aspect: f32,
    pub max_regions: u32,
}

/// Name used by the C header; see [`BlobRegionV2`].
pub type BlobRegionOptionsV2 = RegionOptions;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionError {
    InvalidInput,
    NativeFailure,
}

pub trait RegionDetector: Send {
    fn process(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        options: RegionOptions,
        labels: &mut [u8],
        regions: &mut [Region; MAX_REGIONS],
    ) -> Result<usize, RegionError>;

    fn process_bounded(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        options: RegionOptions,
        max_box_area: f32,
        labels: &mut [u8],
        regions: &mut [Region; MAX_REGIONS],
    ) -> Result<usize, RegionError>;
}

/// Validate the part of the native contract that can be checked from Rust.
/// The native side repeats these checks because the exported C function is a
/// public boundary in its own right.
pub(crate) fn validate_region_input(
    rgba: &[u8],
    width: u32,
    height: u32,
    options: RegionOptions,
    labels: &[u8],
    regions_capacity: usize,
) -> Result<(), RegionError> {
    if !(1..=1024).contains(&width) || !(1..=1024).contains(&height) {
        return Err(RegionError::InvalidInput);
    }

    let Some(pixel_count) = (width as usize).checked_mul(height as usize) else {
        return Err(RegionError::InvalidInput);
    };
    let Some(rgba_len) = pixel_count.checked_mul(4) else {
        return Err(RegionError::InvalidInput);
    };
    if rgba.len() != rgba_len || labels.len() != pixel_count {
        return Err(RegionError::InvalidInput);
    }

    if regions_capacity < MAX_REGIONS || !(1..=MAX_REGIONS as u32).contains(&options.max_regions) {
        return Err(RegionError::InvalidInput);
    }

    if [
        options.threshold,
        options.min_area,
        options.max_area,
        options.min_aspect,
        options.max_aspect,
    ]
    .iter()
    .any(|value| !value.is_finite())
    {
        return Err(RegionError::InvalidInput);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::region_ffi::FfiRegionDetector;
    use std::mem::{offset_of, size_of};

    fn options() -> RegionOptions {
        RegionOptions {
            threshold: 0.5,
            min_area: 0.0,
            max_area: 1.0,
            min_aspect: 0.05,
            max_aspect: 20.0,
            max_regions: 8,
        }
    }

    #[test]
    fn blob_v2_ffi_bounds_and_layout() {
        assert_eq!(size_of::<Region>(), 32);
        assert_eq!(offset_of!(Region, label), 0);
        assert_eq!(offset_of!(Region, x), 4);
        assert_eq!(offset_of!(Region, y), 8);
        assert_eq!(offset_of!(Region, width), 12);
        assert_eq!(offset_of!(Region, height), 16);
        assert_eq!(offset_of!(Region, area), 20);
        assert_eq!(offset_of!(Region, cx), 24);
        assert_eq!(offset_of!(Region, cy), 28);
        assert_eq!(size_of::<RegionOptions>(), 24);
        assert_eq!(offset_of!(RegionOptions, threshold), 0);
        assert_eq!(offset_of!(RegionOptions, min_area), 4);
        assert_eq!(offset_of!(RegionOptions, max_area), 8);
        assert_eq!(offset_of!(RegionOptions, min_aspect), 12);
        assert_eq!(offset_of!(RegionOptions, max_aspect), 16);
        assert_eq!(offset_of!(RegionOptions, max_regions), 20);

        let rgba = vec![0; 4 * 3 * 2];
        let labels = vec![0; 3 * 2];
        let regions = [Region::default(); MAX_REGIONS];
        assert_eq!(
            validate_region_input(&rgba, 3, 2, options(), &labels, regions.len()),
            Ok(())
        );
        assert_eq!(
            validate_region_input(
                &rgba[..rgba.len() - 1],
                3,
                2,
                options(),
                &labels,
                regions.len()
            ),
            Err(RegionError::InvalidInput)
        );
        assert_eq!(
            validate_region_input(&rgba, 0, 2, options(), &labels, regions.len()),
            Err(RegionError::InvalidInput)
        );

        let mut invalid = options();
        invalid.threshold = f32::NAN;
        assert_eq!(
            validate_region_input(&rgba, 3, 2, invalid, &labels, regions.len()),
            Err(RegionError::InvalidInput)
        );
    }

    #[test]
    fn blob_v2_top_k_filters_before_limit() {
        // The fixed ABI must be able to publish all slots. A detector may
        // choose fewer regions with max_regions, but it must never be passed
        // a smaller output array and accidentally truncate before filtering.
        let rgba = vec![0; 4];
        let labels = vec![0; 1];
        let regions = [Region::default(); MAX_REGIONS];
        let mut opts = options();
        opts.max_regions = MAX_REGIONS as u32;
        assert_eq!(
            validate_region_input(&rgba, 1, 1, opts, &labels, regions.len()),
            Ok(())
        );
        assert_eq!(
            validate_region_input(&rgba, 1, 1, opts, &labels, MAX_REGIONS - 1),
            Err(RegionError::InvalidInput)
        );
    }

    fn red_fixture(width: usize, height: usize, pixels: &[(usize, usize)]) -> Vec<u8> {
        let mut rgba = vec![0; width * height * 4];
        for &(x, y) in pixels {
            let index = (y * width + x) * 4;
            rgba[index] = 255;
            rgba[index + 3] = 255;
        }
        rgba
    }

    fn native_options(max_regions: u32) -> RegionOptions {
        RegionOptions {
            threshold: 0.5,
            min_area: 0.0,
            max_area: 1.0,
            min_aspect: 0.0,
            max_aspect: 100.0,
            max_regions,
        }
    }

    #[test]
    fn blob_v2_regions_ring_border_and_diagonal() {
        let mut pixels = Vec::new();
        // A hollow 5x5 ring touching the left border, with a distinct
        // asymmetric location and a 3x3 hole.
        for y in 2..=6 {
            for x in 0..=4 {
                if x == 0 || x == 4 || y == 2 || y == 6 {
                    pixels.push((x, y));
                }
            }
        }
        // A separate 8-connected diagonal component.
        pixels.extend([(6, 0), (7, 1), (6, 2), (7, 3)]);
        // A one-pixel component rejected by min_area.
        pixels.push((7, 7));

        let rgba = red_fixture(8, 8, &pixels);
        let mut detector = FfiRegionDetector::new()
            .expect("rebuilt BlobDetector bundle with V2 symbols is required");
        let mut labels = vec![255; 64];
        let mut regions = [Region::default(); MAX_REGIONS];
        let mut options = native_options(8);
        options.min_area = 0.03;
        let count = detector
            .process(&rgba, 8, 8, options, &mut labels, &mut regions)
            .expect("ring fixture should process");

        assert_eq!(count, 2);
        assert_eq!(regions[0].label, 1);
        assert_eq!(regions[1].label, 2);
        assert_eq!(regions[0].x, 0.0);
        assert_eq!(regions[0].y, 0.25);
        assert_eq!(regions[0].width, 0.625);
        assert_eq!(regions[0].height, 0.625);
        assert!((regions[0].area - 16.0 / 64.0).abs() < f32::EPSILON);
        assert_eq!(labels[3 * 8], 1);
        assert_eq!(labels[3 * 8 + 3], 0);
        assert_eq!(labels[6], 2);
        assert_eq!(labels[7 * 8 + 7], 0);
    }

    #[test]
    fn blob_v2_native_top_k_filters_before_limit() {
        let mut pixels = Vec::new();
        // Four disconnected filled squares, deliberately ordered away from
        // their area ranking. The two smaller components must be rejected
        // after filtering and before max_regions is applied.
        pixels.extend((0..4).flat_map(|y| (0..4).map(move |x| (x, y))));
        pixels.extend((0..3).flat_map(|y| (5..8).map(move |x| (x, y))));
        pixels.extend((5..7).flat_map(|y| (0..2).map(move |x| (x, y))));
        pixels.extend([(7, 7)]);

        let rgba = red_fixture(8, 8, &pixels);
        let mut detector = FfiRegionDetector::new()
            .expect("rebuilt BlobDetector bundle with V2 symbols is required");
        let mut labels = vec![0; 64];
        let mut regions = [Region::default(); MAX_REGIONS];
        let count = detector
            .process(&rgba, 8, 8, native_options(2), &mut labels, &mut regions)
            .expect("top-k fixture should process");
        assert_eq!(count, 2);
        assert_eq!(regions[0].area, 16.0 / 64.0);
        assert_eq!(regions[1].area, 9.0 / 64.0);
        assert!(labels.iter().all(|label| *label <= 2));
        assert_eq!(labels[5 * 8], 0);
        assert_eq!(labels[7 * 8 + 7], 0);
    }

    #[test]
    fn blob_v2_bounded_filters_sparse_outline_before_top_k() {
        let mut pixels = Vec::new();
        // A sparse frame-spanning outline has little foreground area but a
        // large enclosing bounding box. It must not consume the top-k slot.
        for y in 0..16 {
            for x in 0..16 {
                if x == 0 || x == 15 || y == 0 || y == 15 {
                    pixels.push((x, y));
                }
            }
        }
        // Two disconnected interior objects remain below the box-area bound.
        pixels.extend((4..6).flat_map(|y| (4..6).map(move |x| (x, y))));
        pixels.extend((10..12).flat_map(|y| (10..12).map(move |x| (x, y))));

        let rgba = red_fixture(16, 16, &pixels);
        let mut detector = FfiRegionDetector::new()
            .expect("rebuilt BlobDetector bundle with bounded V2 symbol is required");
        let mut labels = vec![255; 256];
        let mut regions = [Region::default(); MAX_REGIONS];
        let count = detector
            .process_bounded(
                &rgba,
                16,
                16,
                native_options(1),
                0.25,
                &mut labels,
                &mut regions,
            )
            .expect("bounded outline fixture should process");

        assert_eq!(count, 1);
        assert_eq!(regions[0].label, 1);
        assert_eq!(regions[0].width, 2.0 / 16.0);
        assert_eq!(regions[0].height, 2.0 / 16.0);
        assert!(labels[..16].iter().all(|label| *label == 0));
        assert_eq!(labels[4 * 16 + 4], 1);
        assert!(labels.iter().all(|label| *label <= 1));
    }

    #[test]
    fn blob_v2_bounded_one_matches_legacy_process_and_rejects_invalid_bounds() {
        let pixels = [(1, 1), (1, 2), (2, 1), (2, 2), (6, 5), (6, 6)];
        let rgba = red_fixture(8, 8, &pixels);
        let options = native_options(8);

        let mut legacy = FfiRegionDetector::new()
            .expect("rebuilt BlobDetector bundle with V2 symbols is required");
        let mut bounded = FfiRegionDetector::new()
            .expect("rebuilt BlobDetector bundle with bounded V2 symbol is required");
        let mut legacy_labels = vec![0; 64];
        let mut bounded_labels = vec![0; 64];
        let mut legacy_regions = [Region::default(); MAX_REGIONS];
        let mut bounded_regions = [Region::default(); MAX_REGIONS];

        let legacy_count = legacy
            .process(
                &rgba,
                8,
                8,
                options,
                &mut legacy_labels,
                &mut legacy_regions,
            )
            .expect("legacy V2 process should process");
        let bounded_count = bounded
            .process_bounded(
                &rgba,
                8,
                8,
                options,
                1.0,
                &mut bounded_labels,
                &mut bounded_regions,
            )
            .expect("permissive bounded process should process");
        assert_eq!(bounded_count, legacy_count);
        assert_eq!(bounded_labels, legacy_labels);
        assert_eq!(bounded_regions, legacy_regions);

        for invalid_bound in [f32::NAN, f32::NEG_INFINITY, -0.01, 1.01] {
            let mut labels = vec![255; 64];
            let mut regions = [Region {
                label: 7,
                ..Region::default()
            }; MAX_REGIONS];
            assert_eq!(
                bounded.process_bounded(
                    &rgba,
                    8,
                    8,
                    options,
                    invalid_bound,
                    &mut labels,
                    &mut regions,
                ),
                Err(RegionError::InvalidInput)
            );
            assert!(labels.iter().all(|label| *label == 0));
            assert!(regions.iter().all(|region| *region == Region::default()));
        }
    }
}
