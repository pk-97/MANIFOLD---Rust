#![allow(private_interfaces)]

//! `node.track_persist` — greedy nearest-neighbour identity tracking
//! with grace-period retention for sparse detection arrays.
//!
//! Each frame, matches incoming detections against a persistent
//! tracked set using Euclidean distance on (X, Y). Matched tracks
//! update their position/size; unmatched detections spawn new tracks
//! (up to capacity); tracks that go unmatched for `grace_frames`
//! consecutive cycles are removed.
//!
//! The output array has stable identity: track N this frame
//! corresponds to track N last frame (unless it was removed and the
//! slot was compacted). This is the prerequisite for temporal filters
//! like `one_euro_filter` — without stable identity, per-element
//! smoothing is meaningless.
//!
//! First consumer: Blob Track. Reusable for any sparse-position
//! detection stream (face boxes, hand regions, motion blobs).

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MAX_TRACKED: usize = 32;

#[derive(Clone, Copy, Default)]
struct TrackedItem {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    matched: bool,
    missed_count: u32,
}

crate::primitive! {
    name: TrackPersist,
    type_id: "node.track_persist",
    purpose: "Greedy nearest-neighbour identity tracking with grace-period retention. Matches incoming Channels[X, Y, WIDTH, HEIGHT] detections against a persistent tracked set using Euclidean distance on (X, Y). Output has stable identity across frames — prerequisite for temporal filters like one_euro_filter. Unmatched detections spawn new tracks (up to capacity); tracks missing for grace_frames cycles are removed.",
    inputs: {
        in: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        match_radius: ScalarF32 optional,
    },
    outputs: {
        out: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("match_radius"),
            label: "Match Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.283),
            range: Some((0.01, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("grace_frames"),
            label: "Grace Frames",
            ty: ParamType::Int,
            default: ParamValue::Float(3.0),
            range: Some((0.0, 30.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire between blob_detect_ffi and one_euro_filter. match_radius is the maximum Euclidean distance (in normalised 0..1 coords) for a detection to claim an existing track — raise for fast-moving blobs, lower for dense scenes. grace_frames controls how long an unmatched track persists before removal — raise for intermittent detections, lower for responsive cleanup. Output capacity matches input capacity; zero-filled slots beyond the active tracked count.",
    examples: [],
    picker: { label: "Track Persist", category: Driver },
    summary: "Keeps a stable identity on each tracked blob from frame to frame, holding onto one briefly even if it flickers out. Stops IDs from jumping around.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["track persist", "identity", "tracking", "id smoothing"],
    boundary_reason: NonGpu,
    extra_fields: {
        tracked: Vec<TrackedItem> = Vec::new(),
        tracked_count: usize = 0,
        ran_once: bool = false,
    },
}

impl Primitive for TrackPersist {
    // Data-driven skip, reporter side: zero live tracks (grace frames
    // included — a track in grace still counts) reports empty, so
    // downstream `empty_skip_input_ports` declarers can skip. The
    // tracker itself always runs (aging is its job).
    fn reports_empty_output(&self) -> bool {
        self.ran_once && self.tracked_count == 0
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let match_radius = ctx.scalar_or_param("match_radius", 0.283);
        let match_radius_sq = match_radius * match_radius;
        let grace_frames = match ctx.params.get("grace_frames") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 3,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let item_size = 16u64; // 4 × f32
        let in_capacity = (in_buf.size / item_size) as usize;
        let out_capacity = (out_buf.size / item_size) as usize;

        let in_ptr = in_buf
            .mapped_ptr()
            .expect("track_persist: input must be shared-memory");
        let in_floats: &[f32] =
            unsafe { std::slice::from_raw_parts(in_ptr as *const f32, in_capacity * 4) };

        // Count active detections (non-zero width+height).
        let mut detection_count = 0usize;
        for i in 0..in_capacity {
            let w = in_floats[i * 4 + 2];
            let h = in_floats[i * 4 + 3];
            if w > 0.0001 || h > 0.0001 {
                detection_count = i + 1;
            }
        }

        // Ensure tracked vec is large enough.
        if self.tracked.len() < MAX_TRACKED {
            self.tracked.resize(MAX_TRACKED, TrackedItem::default());
        }

        // Mark all existing tracked items as unmatched.
        for i in 0..self.tracked_count {
            self.tracked[i].matched = false;
        }

        // Greedy NN matching: each detection claims the closest
        // unmatched track within match_radius.
        for d in 0..detection_count {
            let dx = in_floats[d * 4];
            let dy = in_floats[d * 4 + 1];
            let dw = in_floats[d * 4 + 2];
            let dh = in_floats[d * 4 + 3];
            if dw <= 0.0001 && dh <= 0.0001 {
                continue;
            }

            let mut best_dist_sq = match_radius_sq;
            let mut best_idx: i32 = -1;

            for t in 0..self.tracked_count {
                if self.tracked[t].matched {
                    continue;
                }
                let ex = self.tracked[t].x - dx;
                let ey = self.tracked[t].y - dy;
                let dist_sq = ex * ex + ey * ey;
                if dist_sq < best_dist_sq {
                    best_dist_sq = dist_sq;
                    best_idx = t as i32;
                }
            }

            if best_idx >= 0 {
                let idx = best_idx as usize;
                self.tracked[idx].x = dx;
                self.tracked[idx].y = dy;
                self.tracked[idx].width = dw;
                self.tracked[idx].height = dh;
                self.tracked[idx].matched = true;
                self.tracked[idx].missed_count = 0;
            } else if self.tracked_count < MAX_TRACKED.min(out_capacity) {
                let idx = self.tracked_count;
                self.tracked_count += 1;
                self.tracked[idx] = TrackedItem {
                    x: dx,
                    y: dy,
                    width: dw,
                    height: dh,
                    matched: true,
                    missed_count: 0,
                };
            }
        }

        // Increment missed_count for unmatched tracks.
        for i in 0..self.tracked_count {
            if !self.tracked[i].matched {
                self.tracked[i].missed_count += 1;
            }
        }

        // Remove tracks that exceeded grace period (compact in place).
        let mut write = 0usize;
        for read in 0..self.tracked_count {
            if self.tracked[read].missed_count <= grace_frames {
                if write != read {
                    self.tracked[write] = self.tracked[read];
                }
                write += 1;
            }
        }
        self.tracked_count = write;

        // Write output: tracked items, then zero-fill remainder.
        let out_ptr = out_buf
            .mapped_ptr()
            .expect("track_persist: output must be shared-memory");
        let out_floats: &mut [f32] =
            unsafe { std::slice::from_raw_parts_mut(out_ptr as *mut f32, out_capacity * 4) };

        for i in 0..self.tracked_count.min(out_capacity) {
            let t = &self.tracked[i];
            out_floats[i * 4] = t.x;
            out_floats[i * 4 + 1] = t.y;
            out_floats[i * 4 + 2] = t.width;
            out_floats[i * 4 + 3] = t.height;
        }
        for i in self.tracked_count..out_capacity {
            out_floats[i * 4] = 0.0;
            out_floats[i * 4 + 1] = 0.0;
            out_floats[i * 4 + 2] = 0.0;
            out_floats[i * 4 + 3] = 0.0;
        }
        self.ran_once = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn track_persist_declares_channels_io_and_params() {
        use crate::node_graph::ports::PortType;
        assert_eq!(TrackPersist::TYPE_ID, "node.track_persist");
        assert_eq!(TrackPersist::INPUTS.len(), 2);
        assert_eq!(TrackPersist::INPUTS[0].name, "in");
        assert!(matches!(TrackPersist::INPUTS[0].ty, PortType::Array(_)));
        assert_eq!(TrackPersist::OUTPUTS.len(), 1);
        assert_eq!(TrackPersist::OUTPUTS[0].name, "out");
        let names: Vec<&str> = TrackPersist::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["match_radius", "grace_frames"]);
    }

    #[test]
    fn track_persist_registers_as_palette_driver() {
        let prim = TrackPersist::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.track_persist");
    }

    #[test]
    fn greedy_nn_matching_assigns_closest() {
        let mut tp = TrackPersist::new();
        tp.tracked.resize(MAX_TRACKED, TrackedItem::default());
        tp.tracked[0] = TrackedItem {
            x: 0.5,
            y: 0.5,
            width: 0.1,
            height: 0.1,
            matched: false,
            missed_count: 0,
        };
        tp.tracked[1] = TrackedItem {
            x: 0.8,
            y: 0.8,
            width: 0.1,
            height: 0.1,
            matched: false,
            missed_count: 0,
        };
        tp.tracked_count = 2;

        // Detection at (0.52, 0.48) should match track 0 (closer).
        let det_x = 0.52_f32;
        let det_y = 0.48_f32;
        let match_radius_sq = 0.08_f32;

        let mut best_dist_sq = match_radius_sq;
        let mut best_idx: i32 = -1;
        for t in 0..tp.tracked_count {
            let ex = tp.tracked[t].x - det_x;
            let ey = tp.tracked[t].y - det_y;
            let dist_sq = ex * ex + ey * ey;
            if dist_sq < best_dist_sq {
                best_dist_sq = dist_sq;
                best_idx = t as i32;
            }
        }
        assert_eq!(best_idx, 0, "detection should match track 0 (closest)");
    }

    #[test]
    fn grace_period_removes_stale_tracks() {
        let mut tp = TrackPersist::new();
        tp.tracked.resize(MAX_TRACKED, TrackedItem::default());
        tp.tracked[0] = TrackedItem {
            x: 0.5,
            y: 0.5,
            width: 0.1,
            height: 0.1,
            matched: false,
            missed_count: 4,
        };
        tp.tracked[1] = TrackedItem {
            x: 0.8,
            y: 0.8,
            width: 0.1,
            height: 0.1,
            matched: true,
            missed_count: 0,
        };
        tp.tracked_count = 2;

        let grace_frames = 3u32;
        let mut write = 0usize;
        for read in 0..tp.tracked_count {
            if tp.tracked[read].missed_count <= grace_frames {
                if write != read {
                    tp.tracked[write] = tp.tracked[read];
                }
                write += 1;
            }
        }
        tp.tracked_count = write;

        assert_eq!(tp.tracked_count, 1, "stale track should be removed");
        assert!(
            (tp.tracked[0].x - 0.8).abs() < 1e-5,
            "surviving track should be the matched one at 0.8",
        );
    }
}
