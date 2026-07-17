//! `node.array_filter_detections` — reject degenerate items from a
//! `Channels[X, Y, WIDTH, HEIGHT]` detection array by size, aspect
//! ratio, and frame-coverage bounds.
//!
//! A detector that finds edge-contour regions (blob_detect_ffi) has no
//! notion of "subject" — it boxes whatever clusters. On low-contrast or
//! horizon-heavy footage that means frame-wide, few-pixels-tall strips
//! (the sky/horizon line) and vertical slivers slip through as
//! "blobs." Those are garbage on any content: a box 40× wider than it
//! is tall is almost never a thing you want a HUD bracket on.
//!
//! This atom sits between the detector and the tracker and passes
//! through only items inside every configured bound:
//!   - `min/max_width`, `min/max_height` (normalised 0..1)
//!   - `min/max_aspect` (aspect = width / height)
//!   - `max_area_frac` (reject if bbox area > this fraction of the frame)
//!
//! Defaults are fully permissive (no filtering) so the atom is
//! transparent until configured — the BlobTracking preset sets the
//! bounds. Output preserves the input Channels type and identity order
//! of surviving items; rejected slots are compacted out and the tail
//! zero-filled.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

crate::primitive! {
    name: ArrayFilterDetections,
    type_id: "node.array_filter_detections",
    purpose: "Reject degenerate items from a Channels[X, Y, WIDTH, HEIGHT] detection array by size, aspect ratio, and frame coverage — all bounds in the detector's normalised 0..1 coordinate space. Keeps an item only when min_width <= width <= max_width, min_height <= height <= max_height, min_aspect <= (width / height) <= max_aspect, and (width * height) <= max_area_frac. Drops frame-wide horizon strips (huge aspect), vertical slivers (tiny aspect), specks (below min size), and bbox-covers-everything regions (above max_area_frac). Sits between a detector and a tracker so identity tracking never locks onto garbage. Defaults (width/height bounded to [0,1], min_aspect=0, max_aspect=1000, max_area_frac=1) pass everything through; set bounds to taste. Output preserves the Channels type and the order of surviving items; rejected slots compact out and the tail zero-fills.",
    inputs: {
        in: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        min_height: ScalarF32 optional,
        max_aspect: ScalarF32 optional,
        max_area_frac: ScalarF32 optional,
    },
    outputs: {
        out: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("min_width"),
            label: "Min Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_width"),
            label: "Max Width",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("min_height"),
            label: "Min Height",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_height"),
            label: "Max Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("min_aspect"),
            label: "Min Aspect",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_aspect"),
            label: "Max Aspect",
            ty: ParamType::Float,
            default: ParamValue::Float(1000.0),
            range: Some((0.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_area_frac"),
            label: "Max Area Fraction",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blob_detect_ffi's `blobs` output → this primitive's `in`, and this primitive's `out` → track_persist's `in`. Bounds are in the detector's normalised 0..1 coordinate space. For naturalistic camera footage, max_aspect ~6 + min_height ~0.02 rejects horizon strips while keeping people-sized boxes; max_area_frac 0.5 reproduces the old hardcoded plugin reject (bbox covering >50% of frame). min_height / max_aspect / max_area_frac are port-shadowed so a control can drive them live.",
    examples: [],
    picker: { label: "Filter Detections", category: Driver },
    summary: "Drops junk detections that are too small, too stretched, or cover too much of the frame, before they reach the tracker. Stops a HUD from locking onto the horizon line or a stray sliver.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["filter detections", "reject blobs", "blob filter", "cull detections", "aspect filter"],
    boundary_reason: NonGpu,
    extra_fields: {
        last_kept_count: Option<usize> = None,
    },
}

impl Primitive for ArrayFilterDetections {
    // Data-driven skip, reporter side: a frame that kept zero items
    // reports empty so downstream `empty_skip_input_ports` declarers
    // (the tracker / Draw atoms) can skip to a zero-cost passthrough.
    fn reports_empty_output(&self) -> bool {
        self.last_kept_count == Some(0)
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
        self.last_kept_count = None;
        let min_width = match ctx.params.get("min_width") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let max_width = match ctx.params.get("max_width") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let min_height = ctx.scalar_or_param("min_height", 0.0);
        let max_height = match ctx.params.get("max_height") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let min_aspect = match ctx.params.get("min_aspect") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let max_aspect = ctx.scalar_or_param("max_aspect", 1000.0);
        let max_area_frac = ctx.scalar_or_param("max_area_frac", 1.0);

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
            .expect("array_filter_detections: input must be shared-memory");
        let in_floats: &[f32] =
            unsafe { std::slice::from_raw_parts(in_ptr as *const f32, in_capacity * 4) };

        let out_ptr = out_buf
            .mapped_ptr()
            .expect("array_filter_detections: output must be shared-memory");
        let out_floats: &mut [f32] =
            unsafe { std::slice::from_raw_parts_mut(out_ptr as *mut f32, out_capacity * 4) };

        let mut kept = 0usize;
        for i in 0..in_capacity {
            if kept >= out_capacity {
                break;
            }
            let x = in_floats[i * 4];
            let y = in_floats[i * 4 + 1];
            let w = in_floats[i * 4 + 2];
            let h = in_floats[i * 4 + 3];

            // Sentinel / empty slot — stop, the tail is zero-filled.
            if w <= 0.0001 && h <= 0.0001 {
                continue;
            }

            if w < min_width || w > max_width || h < min_height || h > max_height {
                continue;
            }

            // aspect = width / height; guard a degenerate zero height.
            if h > 0.0001 {
                let aspect = w / h;
                if aspect < min_aspect || aspect > max_aspect {
                    continue;
                }
            }

            if w * h > max_area_frac {
                continue;
            }

            out_floats[kept * 4] = x;
            out_floats[kept * 4 + 1] = y;
            out_floats[kept * 4 + 2] = w;
            out_floats[kept * 4 + 3] = h;
            kept += 1;
        }

        // Zero-fill the remainder so downstream consumers see a clean tail.
        for i in kept..out_capacity {
            out_floats[i * 4] = 0.0;
            out_floats[i * 4 + 1] = 0.0;
            out_floats[i * 4 + 2] = 0.0;
            out_floats[i * 4 + 3] = 0.0;
        }
        self.last_kept_count = Some(kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn array_filter_detections_declares_channels_io() {
        use crate::node_graph::ports::PortType;
        assert_eq!(
            ArrayFilterDetections::TYPE_ID,
            "node.array_filter_detections"
        );
        assert_eq!(ArrayFilterDetections::INPUTS[0].name, "in");
        assert!(matches!(
            ArrayFilterDetections::INPUTS[0].ty,
            PortType::Array(_)
        ));
        assert_eq!(ArrayFilterDetections::OUTPUTS.len(), 1);
        assert_eq!(ArrayFilterDetections::OUTPUTS[0].name, "out");
        let names: Vec<&str> =
            ArrayFilterDetections::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "min_width",
                "max_width",
                "min_height",
                "max_height",
                "min_aspect",
                "max_aspect",
                "max_area_frac"
            ]
        );
    }

    #[test]
    fn array_filter_detections_registers() {
        let prim = ArrayFilterDetections::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.array_filter_detections");
    }

    // Pure filtering-predicate check, mirroring the run() loop's logic
    // so the bound arithmetic is covered without a GPU array context.
    fn passes(
        w: f32,
        h: f32,
        min_w: f32,
        max_w: f32,
        min_h: f32,
        max_h: f32,
        min_a: f32,
        max_a: f32,
        max_area: f32,
    ) -> bool {
        if w <= 0.0001 && h <= 0.0001 {
            return false;
        }
        if w < min_w || w > max_w || h < min_h || h > max_h {
            return false;
        }
        if h > 0.0001 {
            let aspect = w / h;
            if aspect < min_a || aspect > max_a {
                return false;
            }
        }
        if w * h > max_area {
            return false;
        }
        true
    }

    #[test]
    fn rejects_horizon_strip_keeps_subject() {
        // Horizon strip: very wide, few pixels tall → aspect ~40.
        assert!(
            !passes(0.8, 0.02, 0.02, 1.0, 0.02, 1.0, 0.167, 6.0, 0.5),
            "wide flat horizon strip should be rejected by max_aspect"
        );
        // A person-sized box: roughly 0.2 × 0.6 → aspect 0.33.
        assert!(
            passes(0.2, 0.6, 0.02, 1.0, 0.02, 1.0, 0.167, 6.0, 0.5),
            "person-sized box should pass"
        );
    }

    #[test]
    fn rejects_oversized_and_sliver() {
        // bbox covering 64% of the frame → rejected by max_area_frac 0.5.
        assert!(!passes(0.8, 0.8, 0.0, 1.0, 0.0, 1.0, 0.0, 1000.0, 0.5));
        // Vertical sliver: aspect 0.05 → rejected by min_aspect.
        assert!(!passes(0.01, 0.2, 0.0, 1.0, 0.0, 1.0, 0.167, 6.0, 1.0));
    }
}
