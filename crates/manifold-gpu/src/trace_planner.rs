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

/// Hard ceiling on one trace command buffer's GPU time. The macOS GPU
/// watchdog kills a command buffer that runs for seconds; this stays well over
/// an order of magnitude under it. A measured buffer past it restarts the
/// budget from small tiles.
pub const TRACE_BUFFER_CEILING_SECONDS: f64 = 0.1;

/// GPU time one scene layer's frame may take before it counts as a runaway
/// frame. The window server shares the GPU, so a frame this long already
/// stalls the whole Mac; it is far above any frame a show plays at speed and
/// under the watchdog.
pub const RUNAWAY_LAYER_FRAME_SECONDS: f64 = 0.25;

/// Runaway frames in a row before the layer stops drawing, so one slow frame
/// (a first upload, a pipeline build) never stops it.
pub const RUNAWAY_LAYER_FRAMES: u32 = 3;

/// GPU time each trace tile is sized to. Half a 60 fps frame keeps one buffer
/// from starving presentation, and leaves room for the cost per pixel to jump
/// between measurements without reaching the ceiling.
pub const TRACE_BUFFER_TARGET_SECONDS: f64 = 0.008;

/// Pixels per tile before any measurement: a 128x128 block. Even with every
/// ray at the candidate-walk cap this stays far under the ceiling.
pub const TRACE_RESTART_PIXELS_PER_TILE: u64 = 128 * 128;

/// Growth per measurement round is bounded so one quiet frame cannot jump
/// tiles straight to the unit cap.
const TRACE_TILE_MAX_GROWTH: u64 = 4;

/// Sizes trace tiles from measured GPU time. The existing unit and pixel caps
/// in [`DEFAULT_TRACE_WORK_LIMITS`] stay the upper bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceTimeBudget {
    /// Worst measured GPU seconds per traced pixel; zero until measured.
    seconds_per_pixel: f64,
    pixels_per_tile: u64,
    /// Longest measured buffer since the last restart.
    worst_buffer_seconds: f64,
}

impl Default for TraceTimeBudget {
    fn default() -> Self {
        Self::restarted()
    }
}

impl TraceTimeBudget {
    pub const fn restarted() -> Self {
        Self {
            seconds_per_pixel: 0.0,
            pixels_per_tile: TRACE_RESTART_PIXELS_PER_TILE,
            worst_buffer_seconds: 0.0,
        }
    }

    /// Forget every measurement and start again from small tiles.
    pub fn restart(&mut self) {
        *self = Self::restarted();
    }

    pub fn pixels_per_tile(&self) -> u64 {
        self.pixels_per_tile
    }

    pub fn worst_buffer_seconds(&self) -> f64 {
        self.worst_buffer_seconds
    }

    /// Limits for the next trace: the time-sized pixel count, never above
    /// the default caps.
    pub fn limits(&self) -> TraceWorkLimits {
        TraceWorkLimits {
            max_query_units_per_tile: DEFAULT_TRACE_WORK_LIMITS.max_query_units_per_tile,
            max_pixels_per_tile: self
                .pixels_per_tile
                .clamp(1, DEFAULT_TRACE_WORK_LIMITS.max_pixels_per_tile),
        }
    }

    /// Fold in one round of completed buffers: the worst seconds per pixel
    /// among them and the longest buffer. Costs rise at once and fall by at
    /// most half per round, so the estimate follows the worst tiles.
    pub fn observe(&mut self, worst_seconds_per_pixel: f64, longest_buffer_seconds: f64) {
        if !(worst_seconds_per_pixel.is_finite() && worst_seconds_per_pixel > 0.0) {
            return;
        }
        self.worst_buffer_seconds = self.worst_buffer_seconds.max(longest_buffer_seconds);
        self.seconds_per_pixel = if worst_seconds_per_pixel >= self.seconds_per_pixel {
            worst_seconds_per_pixel
        } else {
            worst_seconds_per_pixel.max(self.seconds_per_pixel * 0.5)
        };
        let sized = (TRACE_BUFFER_TARGET_SECONDS / self.seconds_per_pixel).max(1.0);
        let sized = if sized >= u64::MAX as f64 { u64::MAX } else { sized as u64 };
        let grown = self.pixels_per_tile.saturating_mul(TRACE_TILE_MAX_GROWTH);
        self.pixels_per_tile = sized.min(grown).min(DEFAULT_TRACE_WORK_LIMITS.max_pixels_per_tile);
        if longest_buffer_seconds > TRACE_BUFFER_CEILING_SECONDS {
            self.pixels_per_tile = self.pixels_per_tile.min(TRACE_RESTART_PIXELS_PER_TILE);
        }
    }
}

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
    fn time_budget_starts_small_and_never_exceeds_the_default_caps() {
        let budget = TraceTimeBudget::restarted();
        assert_eq!(budget.limits().max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
        assert_eq!(
            budget.limits().max_query_units_per_tile,
            DEFAULT_TRACE_WORK_LIMITS.max_query_units_per_tile
        );
        let mut cheap = TraceTimeBudget::restarted();
        for _ in 0..8 {
            cheap.observe(1e-12, 1e-6);
        }
        assert_eq!(cheap.pixels_per_tile(), DEFAULT_TRACE_WORK_LIMITS.max_pixels_per_tile);
    }

    #[test]
    fn time_budget_sizes_tiles_to_the_target_and_grows_at_most_four_times_per_round() {
        let mut budget = TraceTimeBudget::restarted();
        // 1e-8 s per pixel sizes 800k pixels; growth caps the first round.
        budget.observe(1e-8, 1e-8 * TRACE_RESTART_PIXELS_PER_TILE as f64);
        assert_eq!(budget.pixels_per_tile(), TRACE_RESTART_PIXELS_PER_TILE * 4);
        budget.observe(1e-8, 1e-3);
        assert_eq!(budget.pixels_per_tile(), DEFAULT_TRACE_WORK_LIMITS.max_pixels_per_tile);
        // 1e-6 s per pixel sizes exactly target / cost.
        let mut slow = TraceTimeBudget::restarted();
        slow.observe(1e-6, 1e-3);
        assert_eq!(slow.pixels_per_tile(), (TRACE_BUFFER_TARGET_SECONDS / 1e-6) as u64);
        assert!(slow.pixels_per_tile() as f64 * 1e-6 <= TRACE_BUFFER_TARGET_SECONDS);
    }

    #[test]
    fn time_budget_shrinks_at_once_and_recovers_by_halves() {
        let mut budget = TraceTimeBudget::restarted();
        budget.observe(1e-7, 1e-3);
        let before = budget.pixels_per_tile();
        budget.observe(1e-5, 1e-3);
        assert_eq!(budget.pixels_per_tile(), (TRACE_BUFFER_TARGET_SECONDS / 1e-5) as u64);
        assert!(budget.pixels_per_tile() < before);
        // A cheaper round halves the estimate at most.
        budget.observe(1e-9, 1e-4);
        assert_eq!(budget.pixels_per_tile(), (TRACE_BUFFER_TARGET_SECONDS / 5e-6) as u64);
    }

    #[test]
    fn time_budget_ceiling_breach_returns_to_small_tiles() {
        let mut budget = TraceTimeBudget::restarted();
        for _ in 0..4 {
            budget.observe(1e-10, 1e-4);
        }
        assert!(budget.pixels_per_tile() > TRACE_RESTART_PIXELS_PER_TILE);
        budget.observe(1e-10, TRACE_BUFFER_CEILING_SECONDS * 2.0);
        assert!(budget.pixels_per_tile() <= TRACE_RESTART_PIXELS_PER_TILE);
        assert_eq!(budget.worst_buffer_seconds(), TRACE_BUFFER_CEILING_SECONDS * 2.0);
        budget.restart();
        assert_eq!(budget, TraceTimeBudget::restarted());
    }

    #[test]
    fn time_budget_ignores_unusable_measurements() {
        let mut budget = TraceTimeBudget::restarted();
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            budget.observe(bad, 1.0);
        }
        assert_eq!(budget, TraceTimeBudget::restarted());
    }

    #[test]
    fn time_budget_splits_a_native_4k_trace_into_bounded_tiles() {
        // Export traces High quality at native resolution: the plan must
        // split into as many tiles as the budget needs, each within it.
        let mut budget = TraceTimeBudget::restarted();
        budget.observe(2e-7, 1e-3);
        let limits = budget.limits();
        let regions: Vec<_> = plan_trace_regions(3840, 2160, 8, 8, 40, limits).unwrap().collect();
        assert!(regions.len() > 1);
        let mut covered = 0u64;
        for region in &regions {
            let pixels = u64::from(region.extent[0]) * u64::from(region.extent[1]);
            assert!(pixels <= limits.max_pixels_per_tile);
            assert!(pixels as f64 * 2e-7 <= TRACE_BUFFER_TARGET_SECONDS);
            covered += pixels;
        }
        assert_eq!(covered, 3840 * 2160);
    }

    #[test]
    fn very_large_limits_remain_image_bounded() {
        let plan =
            plan_trace_regions(u32::MAX, u32::MAX, 1, 1, 1, limits(u64::MAX, u64::MAX)).unwrap();
        assert_eq!(plan.tile_width, u32::MAX);
        assert_eq!(plan.tile_height, u32::MAX);
    }
}
