//! `node.track_regions` — bounded identity tracking for V2 region records.
//!
//! This is a CPU boundary because detector records arrive through mapped
//! shared-memory Channels buffers. The tracker owns fixed-size state and
//! assignment scratch; it never allocates on the frame path.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

use super::region_types::{LegacyBox, MAX_REGIONS, Region, TrackRecord};

const MAX_DISTANCE_IN_HEIGHTS: f32 = 0.15;
const MIN_AREA_RATIO: f32 = 0.25;
const MAX_AREA_RATIO: f32 = 4.0;
const EPSILON: f32 = 1.0e-6;

#[derive(Clone, Copy, Debug, Default)]
struct Detection {
    label: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    area: f32,
    cx: f32,
    cy: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct TrackSlot {
    id: u32,
    label: u32,
    observed: u32,
    age: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    cx: f32,
    cy: f32,
    vx: f32,
    vy: f32,
    area: f32,
    raw_cx: f32,
    raw_cy: f32,
    unmatched_seconds: f32,
}

const EMPTY_SLOT: TrackSlot = TrackSlot {
    id: 0,
    label: 0,
    observed: 0,
    age: 0.0,
    x: 0.0,
    y: 0.0,
    width: 0.0,
    height: 0.0,
    cx: 0.0,
    cy: 0.0,
    vx: 0.0,
    vy: 0.0,
    area: 0.0,
    raw_cx: 0.0,
    raw_cy: 0.0,
    unmatched_seconds: 0.0,
};

#[derive(Clone, Copy, Debug)]
struct MatchPair {
    cost: f32,
    track_index: usize,
    detection_index: usize,
    track_id: u32,
    detection_label: u32,
}

const EMPTY_PAIR: MatchPair = MatchPair {
    cost: 0.0,
    track_index: 0,
    detection_index: 0,
    track_id: 0,
    detection_label: 0,
};

#[derive(Clone, Copy)]
pub struct TrackerState {
    slots: [TrackSlot; MAX_REGIONS],
    next_id: u32,
    last_run_seconds: Option<f64>,
}

impl TrackerState {
    const fn new() -> Self {
        Self {
            slots: [EMPTY_SLOT; MAX_REGIONS],
            next_id: 1,
            last_run_seconds: None,
        }
    }

    fn clear(&mut self) {
        self.slots = [EMPTY_SLOT; MAX_REGIONS];
        self.next_id = 1;
    }

    fn write_track(&self, slot: &TrackSlot) -> TrackRecord {
        TrackRecord {
            id: slot.id,
            label: if slot.observed != 0 { slot.label } else { 0 },
            observed: slot.observed,
            age: slot.age,
            x: slot.x,
            y: slot.y,
            width: slot.width,
            height: slot.height,
            cx: slot.cx,
            cy: slot.cy,
            vx: slot.vx,
            vy: slot.vy,
            area: if slot.observed != 0 { slot.area } else { 0.0 },
            pad0: 0,
            pad1: 0,
            pad2: 0,
        }
    }

    fn allocate_id(&mut self) -> u32 {
        if self.next_id == u32::MAX {
            // Do not recycle an ID while old slots still exist. The local
            // tracker is intentionally restarted at the overflow boundary.
            self.clear();
        }
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        id
    }

    fn advance(
        &mut self,
        detections: &[Detection; MAX_REGIONS],
        detection_count: usize,
        dt: f32,
        smoothing_seconds: f32,
        retention_seconds: f32,
        frame_aspect: f32,
    ) {
        if self.next_id == u32::MAX {
            self.clear();
        }

        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let smoothing_seconds = if smoothing_seconds.is_finite() {
            smoothing_seconds.max(0.0)
        } else {
            0.0
        };
        let retention_seconds = if retention_seconds.is_finite() {
            retention_seconds.max(0.0)
        } else {
            0.0
        };
        let frame_aspect = if frame_aspect.is_finite() && frame_aspect > EPSILON {
            frame_aspect
        } else {
            1.0
        };
        let alpha = if dt > 0.0 && smoothing_seconds > EPSILON {
            1.0 - (-dt / smoothing_seconds).exp()
        } else {
            1.0
        };

        let was_observed = self.slots.map(|slot| slot.observed != 0);
        for slot in &mut self.slots {
            if slot.id == 0 {
                continue;
            }
            slot.age += dt;
            slot.unmatched_seconds += dt;
            slot.observed = 0;
            slot.label = 0;
        }

        let mut pairs = [EMPTY_PAIR; MAX_REGIONS * MAX_REGIONS];
        let mut pair_count = 0usize;
        for (detection_index, &detection) in detections
            .iter()
            .enumerate()
            .take(detection_count.min(MAX_REGIONS))
        {
            if detection.label == 0 {
                continue;
            }
            for (track_index, &observed) in was_observed.iter().enumerate() {
                let slot = self.slots[track_index];
                if slot.id == 0
                    || slot.area <= EPSILON
                    || (!observed && slot.unmatched_seconds > retention_seconds)
                {
                    continue;
                }
                let prediction_seconds = slot.unmatched_seconds.min(retention_seconds);
                let predicted_cx = slot.cx + slot.vx * prediction_seconds;
                let predicted_cy = slot.cy + slot.vy * prediction_seconds;
                let dx = (detection.cx - predicted_cx) * frame_aspect;
                let dy = detection.cy - predicted_cy;
                let distance = (dx * dx + dy * dy).sqrt();
                if !distance.is_finite() || distance > MAX_DISTANCE_IN_HEIGHTS {
                    continue;
                }

                let area_ratio = detection.area / slot.area;
                if !area_ratio.is_finite()
                    || !(MIN_AREA_RATIO..=MAX_AREA_RATIO).contains(&area_ratio)
                {
                    continue;
                }

                let predicted_x = slot.x + (predicted_cx - slot.cx);
                let predicted_y = slot.y + (predicted_cy - slot.cy);
                let iou = intersection_over_union(
                    predicted_x,
                    predicted_y,
                    slot.width,
                    slot.height,
                    detection.x,
                    detection.y,
                    detection.width,
                    detection.height,
                );
                let cost = 0.65 * (distance / MAX_DISTANCE_IN_HEIGHTS) + 0.35 * (1.0 - iou);
                if cost.is_finite() && pair_count < pairs.len() {
                    pairs[pair_count] = MatchPair {
                        cost,
                        track_index,
                        detection_index,
                        track_id: slot.id,
                        detection_label: detection.label,
                    };
                    pair_count += 1;
                }
            }
        }

        pairs[..pair_count].sort_unstable_by(compare_pairs);
        let mut track_taken = [false; MAX_REGIONS];
        let mut detection_taken = [false; MAX_REGIONS];
        for pair in pairs[..pair_count].iter().copied() {
            if track_taken[pair.track_index] || detection_taken[pair.detection_index] {
                continue;
            }
            track_taken[pair.track_index] = true;
            detection_taken[pair.detection_index] = true;
            update_slot(
                &mut self.slots[pair.track_index],
                detections[pair.detection_index],
                dt,
                alpha,
            );
        }

        // A fresh match never expires merely because the sample interval
        // exceeds Retention. Unmatched tracks retire before spawning so
        // their slots are immediately reusable.
        for slot in &mut self.slots {
            if slot.id != 0 && slot.observed == 0 && slot.unmatched_seconds > retention_seconds {
                *slot = EMPTY_SLOT;
            }
        }

        // Expiration happened before spawning. New detections use the lowest
        // free slot in input order, which is deterministic after assignment.
        for detection_index in 0..detection_count.min(MAX_REGIONS) {
            if detection_taken[detection_index] {
                continue;
            }
            let detection = detections[detection_index];
            if detection.label == 0 {
                continue;
            }
            let Some(slot_index) = self.slots.iter().position(|slot| slot.id == 0) else {
                break;
            };
            let id = self.allocate_id();
            // allocate_id can clear on overflow; the guard at the start makes
            // this branch unreachable while live slots are being assigned.
            self.slots[slot_index] = TrackSlot {
                id,
                label: detection.label,
                observed: 1,
                age: 0.0,
                x: detection.x,
                y: detection.y,
                width: detection.width,
                height: detection.height,
                cx: detection.cx,
                cy: detection.cy,
                vx: 0.0,
                vy: 0.0,
                area: detection.area,
                raw_cx: detection.cx,
                raw_cy: detection.cy,
                unmatched_seconds: 0.0,
            };
            detection_taken[detection_index] = true;
        }
    }
}

fn compare_pairs(a: &MatchPair, b: &MatchPair) -> std::cmp::Ordering {
    a.cost
        .partial_cmp(&b.cost)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.track_id.cmp(&b.track_id))
        .then_with(|| a.detection_label.cmp(&b.detection_label))
        .then_with(|| a.track_index.cmp(&b.track_index))
        .then_with(|| a.detection_index.cmp(&b.detection_index))
}

fn intersection_over_union(
    ax: f32,
    ay: f32,
    aw: f32,
    ah: f32,
    bx: f32,
    by: f32,
    bw: f32,
    bh: f32,
) -> f32 {
    let left = ax.max(bx);
    let top = ay.max(by);
    let right = (ax + aw).min(bx + bw);
    let bottom = (ay + ah).min(by + bh);
    let intersection = (right - left).max(0.0) * (bottom - top).max(0.0);
    let union = (aw * ah + bw * bh - intersection).max(0.0);
    if union > EPSILON {
        (intersection / union).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn update_slot(slot: &mut TrackSlot, detection: Detection, dt: f32, alpha: f32) {
    let measured_vx = if dt > EPSILON {
        (detection.cx - slot.raw_cx) / dt
    } else {
        0.0
    };
    let measured_vy = if dt > EPSILON {
        (detection.cy - slot.raw_cy) / dt
    } else {
        0.0
    };

    slot.x += alpha * (detection.x - slot.x);
    slot.y += alpha * (detection.y - slot.y);
    slot.width += alpha * (detection.width - slot.width);
    slot.height += alpha * (detection.height - slot.height);
    slot.cx += alpha * (detection.cx - slot.cx);
    slot.cy += alpha * (detection.cy - slot.cy);
    slot.vx += alpha * (measured_vx - slot.vx);
    slot.vy += alpha * (measured_vy - slot.vy);
    slot.raw_cx = detection.cx;
    slot.raw_cy = detection.cy;
    slot.label = detection.label;
    slot.observed = 1;
    slot.area = detection.area;
    slot.unmatched_seconds = 0.0;
}

fn detection_from_region(region: Region) -> Option<Detection> {
    if region.label == 0
        || ![
            region.x,
            region.y,
            region.width,
            region.height,
            region.area,
            region.cx,
            region.cy,
        ]
        .iter()
        .all(|value| value.is_finite())
        || region.width <= 0.0
        || region.height <= 0.0
        || region.area <= 0.0
    {
        return None;
    }
    Some(Detection {
        label: region.label,
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
        area: region.area,
        cx: region.cx,
        cy: region.cy,
    })
}

crate::primitive! {
    name: TrackRegions,
    type_id: "node.track_regions",
    purpose: "Track up to 32 connected-component regions with stable runtime-local IDs, velocity and age. Fresh successful samples update identity; stale samples retain the last publication without aging it.",
    inputs: {
        regions: Channels[LABEL: U32, X: F32, Y: F32, WIDTH: F32, HEIGHT: F32, AREA: F32, CX: F32, CY: F32] required,
        updated: ScalarF32 required,
        sample_dt: ScalarF32 required,
        valid: ScalarF32 required,
        reset: ScalarF32 optional,
        frame_aspect: ScalarF32 optional,
    },
    outputs: {
        tracks: Channels[ID: U32, LABEL: U32, OBSERVED: U32, AGE: F32, X: F32, Y: F32, WIDTH: F32, HEIGHT: F32, CX: F32, CY: F32, VX: F32, VY: F32, AREA: F32, PAD0: U32, PAD1: U32, PAD2: U32],
        boxes: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("smoothing_seconds"),
            label: "Smoothing",
            ty: ParamType::Float,
            default: ParamValue::Float(0.06),
            range: Some((0.0, 0.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("retention_seconds"),
            label: "Retention",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.detect_regions `regions`, `updated`, `sample_dt` and `valid` directly into this node. `updated=0` retains the previous publication; a successful empty sample advances age and retention. Runtime-local IDs are stable through brief disappearance, while OBSERVED/LABEL/AREA become zero during a gap. `boxes` is the legacy four-float layout compacted to observed tracks in slot order. `frame_aspect` converts normalized horizontal distance into image-height units.",
    examples: [],
    picker: { label: "Track Regions", category: Driver },
    summary: "Assigns stable IDs and motion to detected regions while preserving the legacy box stream for existing HUD nodes.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["region tracking", "blob track v2", "stable blobs"],
    boundary_reason: NonGpu,
    extra_fields: {
        state: TrackerState = TrackerState::new(),
    },
}

impl Primitive for TrackRegions {
    fn reports_empty_output(&self) -> bool {
        self.state
            .slots
            .iter()
            .all(|slot| slot.id == 0 || slot.observed == 0)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "tracks" | "boxes" => Some(MAX_REGIONS as u32),
            _ => None,
        }
    }

    fn clear_state(&mut self) {
        self.state = TrackerState::new();
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let now = ctx.time.seconds.0;
        let timeline_reset = !now.is_finite()
            || self
                .state
                .last_run_seconds
                .is_some_and(|previous| now < previous || now - previous > 1.0);
        if timeline_reset {
            self.state.clear();
        }
        self.state.last_run_seconds = now.is_finite().then_some(now);

        let Some(regions_buffer) = ctx.inputs.array("regions") else {
            return;
        };
        let tracks_buffer = ctx.outputs.array("tracks");
        let boxes_buffer = ctx.outputs.array("boxes");
        if tracks_buffer.is_none() && boxes_buffer.is_none() {
            return;
        }

        let reset = ctx
            .inputs
            .scalar("reset")
            .and_then(|value| value.as_scalar())
            .unwrap_or(0.0);
        let valid = ctx
            .inputs
            .scalar("valid")
            .and_then(|value| value.as_scalar())
            .unwrap_or(0.0);
        let updated = ctx
            .inputs
            .scalar("updated")
            .and_then(|value| value.as_scalar())
            .unwrap_or(0.0);

        if reset > 0.5 || !valid.is_finite() || valid <= 0.0 {
            self.state.clear();
            write_outputs(&self.state, tracks_buffer, boxes_buffer);
            return;
        }

        if updated > 0.5 && updated.is_finite() {
            let sample_dt = ctx
                .inputs
                .scalar("sample_dt")
                .and_then(|value| value.as_scalar())
                .unwrap_or(0.0);
            let dt_requires_reset = !sample_dt.is_finite() || !(0.0..=1.0).contains(&sample_dt);
            if dt_requires_reset {
                self.state.clear();
            }

            let smoothing_seconds = ctx.param_f32("smoothing_seconds", 0.06).max(0.0);
            let retention_seconds = ctx.param_f32("retention_seconds", 0.15).max(0.0);
            let frame_aspect = ctx.scalar_or_param("frame_aspect", 1.0);

            let region_capacity =
                (regions_buffer.size as usize / std::mem::size_of::<Region>()).min(MAX_REGIONS);
            let mut detections = [Detection::default(); MAX_REGIONS];
            let mut detection_count = 0usize;
            let Some(ptr) = regions_buffer.mapped_ptr() else {
                write_outputs(&self.state, tracks_buffer, boxes_buffer);
                return;
            };
            let regions =
                unsafe { std::slice::from_raw_parts(ptr as *const Region, region_capacity) };
            for &region in regions {
                if let Some(detection) = detection_from_region(region) {
                    if detection_count == MAX_REGIONS {
                        break;
                    }
                    detections[detection_count] = detection;
                    detection_count += 1;
                }
            }

            self.state.advance(
                &detections,
                detection_count,
                if dt_requires_reset { 0.0 } else { sample_dt },
                smoothing_seconds,
                retention_seconds,
                frame_aspect,
            );
        }

        write_outputs(&self.state, tracks_buffer, boxes_buffer);
    }
}

fn write_outputs(
    state: &TrackerState,
    tracks_buffer: Option<&manifold_gpu::GpuBuffer>,
    boxes_buffer: Option<&manifold_gpu::GpuBuffer>,
) {
    if let Some(tracks_buffer) = tracks_buffer
        && let Some(ptr) = tracks_buffer.mapped_ptr()
    {
        let capacity =
            (tracks_buffer.size as usize / std::mem::size_of::<TrackRecord>()).min(MAX_REGIONS);
        let output = unsafe { std::slice::from_raw_parts_mut(ptr as *mut TrackRecord, capacity) };
        for (index, destination) in output.iter_mut().enumerate() {
            *destination = state.write_track(&state.slots[index]);
        }
    }

    if let Some(boxes_buffer) = boxes_buffer
        && let Some(ptr) = boxes_buffer.mapped_ptr()
    {
        let capacity =
            (boxes_buffer.size as usize / std::mem::size_of::<LegacyBox>()).min(MAX_REGIONS);
        let output = unsafe { std::slice::from_raw_parts_mut(ptr as *mut LegacyBox, capacity) };
        let mut written = 0usize;
        for slot in state
            .slots
            .iter()
            .filter(|slot| slot.id != 0 && slot.observed != 0)
        {
            if written == capacity {
                break;
            }
            output[written] = LegacyBox {
                x: slot.x,
                y: slot.y,
                width: slot.width,
                height: slot.height,
            };
            written += 1;
        }
        for destination in &mut output[written..] {
            *destination = LegacyBox::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(label: u32, x: f32, y: f32, width: f32, height: f32, area: f32) -> Region {
        Region {
            label,
            x,
            y,
            width,
            height,
            area,
            cx: x + width * 0.5,
            cy: y + height * 0.5,
        }
    }

    fn sample(state: &mut TrackerState, regions: &[Region], dt: f32) {
        let mut detections = [Detection::default(); MAX_REGIONS];
        let mut count = 0usize;
        for &region in regions {
            if let Some(detection) = detection_from_region(region) {
                detections[count] = detection;
                count += 1;
            }
        }
        state.advance(&detections, count, dt, 0.0, 0.15, 1.0);
    }

    #[test]
    fn blob_v2_tracking_identity_and_gaps() {
        let mut state = TrackerState::new();
        sample(
            &mut state,
            &[
                region(1, 0.1, 0.2, 0.1, 0.1, 0.01),
                region(2, 0.7, 0.2, 0.2, 0.2, 0.04),
            ],
            0.0,
        );
        let first_ids = [state.slots[0].id, state.slots[1].id];
        assert_ne!(first_ids[0], first_ids[1]);

        // Detector ordering is allowed to reverse; cost matching preserves
        // the identities at the same measured positions.
        sample(
            &mut state,
            &[
                region(2, 0.7, 0.2, 0.2, 0.2, 0.04),
                region(1, 0.1, 0.2, 0.1, 0.1, 0.01),
            ],
            0.05,
        );
        assert_eq!(state.slots[0].id, first_ids[0]);
        assert_eq!(state.slots[1].id, first_ids[1]);

        sample(&mut state, &[], 0.05);
        assert_eq!(state.slots[0].observed, 0);
        assert_eq!(state.slots[1].observed, 0);
        assert_eq!(state.slots[0].id, first_ids[0]);
        assert_eq!(state.slots[1].id, first_ids[1]);

        sample(&mut state, &[region(3, 0.11, 0.2, 0.1, 0.1, 0.01)], 0.05);
        assert_eq!(state.slots[0].id, first_ids[0]);
        assert_eq!(state.slots[0].label, 3);
        assert_eq!(state.slots[0].observed, 1);
    }

    #[test]
    fn blob_v2_tracking_merge_split_has_documented_slot_behaviour() {
        let mut state = TrackerState::new();
        sample(
            &mut state,
            &[
                region(1, 0.2, 0.2, 0.1, 0.1, 0.01),
                region(2, 0.4, 0.2, 0.1, 0.1, 0.01),
            ],
            0.0,
        );
        let first_id = state.slots[0].id;
        sample(&mut state, &[region(9, 0.25, 0.2, 0.2, 0.1, 0.02)], 0.05);
        assert_eq!(state.slots[0].id, first_id);
        assert_eq!(state.slots[0].label, 9);
        assert_eq!(state.slots[1].observed, 0);

        sample(
            &mut state,
            &[
                region(10, 0.2, 0.2, 0.1, 0.1, 0.01),
                region(11, 0.4, 0.2, 0.1, 0.1, 0.01),
            ],
            0.05,
        );
        // Prediction after the merge can pull the first identity away from
        // the split. The second retained track takes the right component;
        // the left component starts a new identity rather than pretending
        // that occlusion can be resolved from a single merged observation.
        assert_eq!(state.slots[0].id, first_id);
        assert_eq!(state.slots[0].observed, 0);
        assert_eq!(state.slots[1].label, 11);
        assert_ne!(state.slots[2].id, first_id);
        assert_eq!(state.slots[2].label, 10);
    }

    #[test]
    fn blob_v2_successful_empty_sample_ages_and_expires() {
        let mut state = TrackerState::new();
        sample(&mut state, &[region(1, 0.2, 0.2, 0.1, 0.1, 0.01)], 0.0);
        sample(&mut state, &[], 0.10);
        assert!(state.slots[0].age >= 0.10);
        assert_ne!(state.slots[0].id, 0);
        sample(&mut state, &[], 0.06);
        assert_eq!(state.slots[0].id, 0);
    }
}
