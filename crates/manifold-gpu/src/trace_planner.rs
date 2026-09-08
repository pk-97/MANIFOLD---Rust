//! Deterministic tiling for ray-trace work.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceWorkLimits {
    pub max_query_units_per_tile: u64,
    pub max_pixels_per_tile: u64,
}

/// One scheduling policy for every RT trace, independent of whether the
/// caller is rendering interactively or exporting.
pub const DEFAULT_TRACE_WORK_LIMITS: TraceWorkLimits = TraceWorkLimits {
    max_query_units_per_tile: 1 << 24,
    max_pixels_per_tile: 1 << 18,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct TraceRegion {
    pub origin: [u32; 2],
    pub extent: [u32; 2],
}

impl TraceRegion {
    pub const fn full(size: [u32; 2]) -> Self {
        Self {
            origin: [0, 0],
            extent: size,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TracePlanError {
    ZeroDimension,
    ZeroWorkgroup,
    ZeroQueryUnits,
    ZeroLimit,
    ArithmeticOverflow,
    WorkgroupExceedsLimits,
}

/// Conservative upper bound on acceleration-structure queries issued by one
/// valid trace pixel. This describes work; it never changes requested quality.
#[allow(clippy::too_many_arguments)]
pub fn estimate_trace_query_units_per_pixel(
    caster_count: u32,
    sun_count: u32,
    shadow_spp: u32,
    ao_spp: u32,
    gi_spp: u32,
    reflection_spp: u32,
    emissive_direct: bool,
) -> Result<u64, TracePlanError> {
    const GI_BOUNCES: u64 = 2;

    let caster_count = u64::from(caster_count);
    let sun_count = u64::from(sun_count);
    let shadow = caster_count
        .checked_mul(u64::from(shadow_spp))
        .ok_or(TracePlanError::ArithmeticOverflow)?;
    let primary = u64::from(ao_spp != 0 || gi_spp != 0)
        + u64::from(reflection_spp != 0);
    let ao = u64::from(ao_spp);
    let per_gi_sample = GI_BOUNCES
        .checked_mul(
            1u64.checked_add(sun_count)
                .ok_or(TracePlanError::ArithmeticOverflow)?,
        )
        .ok_or(TracePlanError::ArithmeticOverflow)?;
    let gi = u64::from(gi_spp)
        .checked_mul(per_gi_sample)
        .ok_or(TracePlanError::ArithmeticOverflow)?;
    let reflection = u64::from(reflection_spp)
        .checked_mul(
            1u64.checked_add(sun_count)
                .ok_or(TracePlanError::ArithmeticOverflow)?,
        )
        .ok_or(TracePlanError::ArithmeticOverflow)?;

    shadow
        .checked_add(primary)
        .and_then(|v| v.checked_add(ao))
        .and_then(|v| v.checked_add(gi))
        .and_then(|v| v.checked_add(reflection))
        .and_then(|v| v.checked_add(u64::from(emissive_direct && gi_spp != 0)))
        .ok_or(TracePlanError::ArithmeticOverflow)
        .map(|units| units.max(1))
}

#[derive(Clone, Copy, Debug)]
pub struct TraceRegionIter {
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    x: u32,
    y: u32,
}

/// Plan a row-major exact cover of the trace image without allocating.
pub fn plan_trace_regions(
    width: u32,
    height: u32,
    workgroup_width: u32,
    workgroup_height: u32,
    query_units_per_pixel: u64,
    limits: TraceWorkLimits,
) -> Result<TraceRegionIter, TracePlanError> {
    if width == 0 || height == 0 {
        return Err(TracePlanError::ZeroDimension);
    }
    if workgroup_width == 0 || workgroup_height == 0 {
        return Err(TracePlanError::ZeroWorkgroup);
    }
    if query_units_per_pixel == 0 {
        return Err(TracePlanError::ZeroQueryUnits);
    }
    if limits.max_query_units_per_tile == 0 || limits.max_pixels_per_tile == 0 {
        return Err(TracePlanError::ZeroLimit);
    }

    let active_group_width = workgroup_width.min(width);
    let active_group_height = workgroup_height.min(height);
    let active_group_pixels = u64::from(active_group_width)
        .checked_mul(u64::from(active_group_height))
        .ok_or(TracePlanError::ArithmeticOverflow)?;
    let query_for_workgroup = active_group_pixels
        .checked_mul(query_units_per_pixel)
        .ok_or(TracePlanError::ArithmeticOverflow)?;
    if query_for_workgroup > limits.max_query_units_per_tile
        || active_group_pixels > limits.max_pixels_per_tile
    {
        return Err(TracePlanError::WorkgroupExceedsLimits);
    }

    // Use the greatest horizontal run that fits, then fill one or more
    // aligned rows. This is deterministic and keeps every non-edge tile
    // aligned to the requested workgroup.
    let query_capacity = limits.max_query_units_per_tile / query_units_per_pixel;
    let pixel_capacity = query_capacity.min(limits.max_pixels_per_tile);
    let max_groups_w = u64::from(width).div_ceil(u64::from(workgroup_width));
    let groups_w = (pixel_capacity / active_group_pixels)
        .max(1)
        .min(max_groups_w);
    let tile_width_u64 = u64::from(workgroup_width)
        .checked_mul(groups_w)
        .ok_or(TracePlanError::ArithmeticOverflow)?
        .min(u64::from(width));
    let tile_width =
        u32::try_from(tile_width_u64).map_err(|_| TracePlanError::ArithmeticOverflow)?;
    let row_pixels = u64::from(tile_width.min(width))
        .checked_mul(u64::from(active_group_height))
        .ok_or(TracePlanError::ArithmeticOverflow)?;
    let max_groups_h = u64::from(height).div_ceil(u64::from(workgroup_height));
    let groups_h = (pixel_capacity / row_pixels).max(1).min(max_groups_h);
    let tile_height_u64 = u64::from(workgroup_height)
        .checked_mul(groups_h.max(1))
        .ok_or(TracePlanError::ArithmeticOverflow)?
        .min(u64::from(height));
    let tile_height =
        u32::try_from(tile_height_u64).map_err(|_| TracePlanError::ArithmeticOverflow)?;

    Ok(TraceRegionIter {
        width,
        height,
        tile_width,
        tile_height,
        x: 0,
        y: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(query: u64, pixels: u64) -> TraceWorkLimits {
        TraceWorkLimits {
            max_query_units_per_tile: query,
            max_pixels_per_tile: pixels,
        }
    }

    #[test]
    fn single_tile() {
        let plan = plan_trace_regions(8, 4, 4, 2, 3, limits(96, 32)).unwrap();
        assert_eq!(
            plan.collect::<Vec<_>>(),
            vec![TraceRegion {
                origin: [0, 0],
                extent: [8, 4]
            }]
        );
    }

    #[test]
    fn multi_tile_is_exact_cover_without_overlap() {
        let regions: Vec<_> = plan_trace_regions(10, 7, 2, 2, 1, limits(12, 12))
            .unwrap()
            .collect();
        let mut covered = vec![false; 10 * 7];
        for region in regions {
            for y in region.origin[1]..region.origin[1] + region.extent[1] {
                for x in region.origin[0]..region.origin[0] + region.extent[0] {
                    let slot = &mut covered[y as usize * 10 + x as usize];
                    assert!(!*slot);
                    *slot = true;
                }
            }
        }
        assert!(covered.into_iter().all(|cell| cell));
    }

    #[test]
    fn edge_tiles_are_partial() {
        let regions: Vec<_> = plan_trace_regions(5, 5, 2, 2, 1, limits(4, 4))
            .unwrap()
            .collect();
        assert!(regions.iter().any(|r| r.extent == [1, 2]));
        assert!(regions.iter().any(|r| r.extent == [2, 1]));
        assert!(regions.iter().any(|r| r.extent == [1, 1]));
    }

    #[test]
    fn tight_limits_allow_one_workgroup() {
        let plan = plan_trace_regions(4, 4, 2, 2, 5, limits(20, 4)).unwrap();
        assert_eq!(plan.tile_width, 2);
        assert_eq!(plan.tile_height, 2);
    }

    #[test]
    fn invalid_and_overflow_inputs_are_rejected() {
        let base = limits(u64::MAX, u64::MAX);
        assert!(matches!(
            plan_trace_regions(0, 1, 1, 1, 1, base),
            Err(TracePlanError::ZeroDimension)
        ));
        assert!(matches!(
            plan_trace_regions(1, 1, 1, 1, 0, base),
            Err(TracePlanError::ZeroQueryUnits)
        ));
        assert!(matches!(
            plan_trace_regions(u32::MAX, u32::MAX, u32::MAX, u32::MAX, 2, base),
            Err(TracePlanError::ArithmeticOverflow)
        ));
    }

    #[test]
    fn image_smaller_than_workgroup_is_one_partial_tile() {
        let regions: Vec<_> = plan_trace_regions(1, 3, 8, 8, 7, limits(21, 3))
            .unwrap()
            .collect();
        assert_eq!(regions, vec![TraceRegion::full([1, 3])]);
    }

    #[test]
    fn query_estimate_preserves_requested_sample_counts() {
        // shadow=2*4, primary=1, AO=3, GI=2*2*(1+1 sun),
        // reflection=5*(1+1 sun), emissive direct=1.
        assert_eq!(
            estimate_trace_query_units_per_pixel(2, 1, 4, 3, 2, 5, true).unwrap(),
            32
        );
    }

    #[test]
    fn very_large_limits_remain_image_bounded() {
        let plan =
            plan_trace_regions(u32::MAX, u32::MAX, 1, 1, 1, limits(u64::MAX, u64::MAX)).unwrap();
        assert_eq!(plan.tile_width, u32::MAX);
        assert_eq!(plan.tile_height, u32::MAX);
    }
}

impl Iterator for TraceRegionIter {
    type Item = TraceRegion;

    fn next(&mut self) -> Option<Self::Item> {
        if self.y >= self.height {
            return None;
        }
        let origin = [self.x, self.y];
        let extent = [
            self.tile_width.min(self.width - self.x),
            self.tile_height.min(self.height - self.y),
        ];
        self.x = self.x.saturating_add(extent[0]);
        if self.x >= self.width {
            self.x = 0;
            self.y = self.y.saturating_add(extent[1]);
        }
        Some(TraceRegion { origin, extent })
    }
}
