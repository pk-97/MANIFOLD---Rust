//! `node.gltf_mesh_source` — read a `.glb`/`.gltf` file from disk and emit
//! its geometry as an `Array(MeshVertex)` wire, so imported meshes flow
//! into `node.render_scene` like any hand-built shape (`node.cube_mesh`,
//! `node.generate_grid_mesh`, …).
//!
//! File I/O + the CPU flatten (`gltf_load::load_gltf_mesh`) happen on a
//! background thread (`std::thread::spawn` + `mpsc::channel`), same
//! pattern as `node.image_folder`, so the content thread never stalls on
//! a multi-megabyte glTF parse. The last successful parse stays resident
//! (`cached_verts`) and re-uploads to a staging buffer only when the
//! parse result actually changes; the GPU copy into the pre-bound output
//! buffer runs every frame via a cheap blit.

use std::borrow::Cow;
use std::sync::mpsc;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_load::{DEFAULT_MATERIAL_MESH_PARAM, GltfMeshSelector, load_gltf_mesh};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// `fit` enum labels (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D7). Index 0
/// (`none`) is the default and a strict no-op — every scan arrives at
/// arbitrary scale/origin; `unit_box` normalizes it so deformer defaults and
/// `node.mesh_ramp`'s 0..1 bounds are meaningful without a separate
/// `normalize_mesh` GPU atom (which would need a same-frame GPU->CPU bounds
/// readback, forbidden by DECOMPOSING §7).
pub const GLTF_FIT_MODES: &[&str] = &["none", "unit_box"];

/// Apply the `fit`/`recenter` transform to a freshly parsed vertex set,
/// D7's parse-time extension. Runs on the background parse thread (not
/// per-frame) — cost is one pass over the vertex count, once per parse or
/// param change, never in `run()`'s per-frame path.
///
/// `fit_unit_box = false` is a STRICT no-op (early return, no float math at
/// all) — this is what keeps every pre-existing gltf preset byte-identical
/// after this extension ships (`fit` defaults to `none`).
fn apply_mesh_fit(verts: Vec<MeshVertex>, fit_unit_box: bool, recenter: bool) -> Vec<MeshVertex> {
    if !fit_unit_box || verts.is_empty() {
        return verts;
    }

    let mut min = verts[0].position;
    let mut max = verts[0].position;
    for v in &verts {
        for axis in 0..3 {
            min[axis] = min[axis].min(v.position[axis]);
            max[axis] = max[axis].max(v.position[axis]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let longest = size[0].max(size[1]).max(size[2]);
    let scale = if longest > 1e-8 { 1.0 / longest } else { 1.0 };

    verts
        .into_iter()
        .map(|mut v| {
            let mut p = v.position;
            for axis in 0..3 {
                let centered = p[axis] - center[axis];
                p[axis] = if recenter {
                    centered * scale
                } else {
                    center[axis] + centered * scale
                };
            }
            v.position = p;
            // Normals unchanged: a uniform scale doesn't change a vector's
            // direction, only its magnitude, and this is applied once at
            // parse time, not renormalized-per-frame like a deformer.
            v
        })
        .collect()
}

crate::primitive! {
    name: GltfMeshSource,
    type_id: "node.gltf_mesh_source",
    purpose: "Read a glTF/.glb file from disk and emit its geometry as a triangle-list Array(MeshVertex) wire. mesh_index=-1 (the default) world-combines the whole default scene — drop a model in and it renders. mesh_index >= 0 selects one mesh (optionally one primitive of it via primitive_index) in LOCAL space, undisplaced by node transforms, for callers that place it themselves via node.render_scene's per-object transform.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("path"),
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mesh_index"),
            label: "Mesh Index",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("primitive_index"),
            label: "Primitive Index",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("material_index"),
            label: "Material Index",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            // -1 (default) = unset, falls through to mesh_index/WholeScene
            // below. -2 is GLB_XFAIL_BURNDOWN_DESIGN.md D4's reserved
            // sentinel (`gltf_load::DEFAULT_MATERIAL_MESH_PARAM`) selecting
            // the glTF default-material (materialless) geometry — real
            // glTF material indices are always >= 0, so widening the range
            // down to -2 costs nothing for every existing selection.
            range: Some((-2.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(200000.0),
            range: Some((36.0, 8000000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fit"),
            label: "Fit",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // none
            range: Some((0.0, (GLTF_FIT_MODES.len() - 1) as f32)),
            enum_values: GLTF_FIT_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("recenter"),
            label: "Recenter",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
        // BUG-194/BUG-195 (SCENE_SETUP_PANEL_DESIGN.md D4/D5): import-time
        // provenance, never read by `evaluate()`/`run()` — stamped by
        // `gltf_import.rs` at import/merge time and read back by
        // `SceneVm::from_def` (the header's vertex-count row) and
        // `merge_import_into_graph` (scale-sanity's reference-radius pick).
        // -1 / -1.0 mean "unknown" (a hand-built node never touched by the
        // importer) — never a fabricated non-negative default.
        ParamDef {
            name: Cow::Borrowed("source_vertex_count"),
            label: "Source Vertex Count",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 8_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("source_bbox_radius"),
            label: "Source Bbox Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(-1.0),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "path comes via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card Browse field into this primitive's `path` param, same convention as node.image_folder's `folder`. mesh_index=-1 means whole scene, world-combined under the default scene's transform hierarchy (the default — \"just drop a model in\"); mesh_index >= 0 plus primitive_index select a single mesh or primitive in LOCAL space so the importer can place it via node.render_scene's per-object pos_*/rot_*/scale_* transforms instead of baking a node transform in. max_capacity is the pre-allocation ceiling in vertices; the glTF importer sets it to the exact parsed vertex count, so manual drops of meshes exceeding it are truncated with a logged warning rather than silently dropping the tail. fit=unit_box uniformly scales the parsed mesh so its longest bounding-box axis is 1.0 — every scan arrives at arbitrary scale, and this makes deformer defaults (push_along_normals amount, mesh_ramp bounds) meaningful without hand-tuning per-asset. recenter (default true, only consulted under fit=unit_box) additionally translates the bounding-box center to the origin; false keeps the box's original center and only rescales around it. fit defaults to `none`, a strict no-op — every pre-existing gltf preset is byte-identical after this param was added. Both apply once on the background parse thread, not per-frame.",
    examples: [],
    picker: { label: "glTF Mesh", category: Atom },
    summary: "Loads a glTF/.glb model file from disk as mesh geometry, so imported 3D assets flow into the render pipeline like any other shape primitive.",
    category: Geometry3D,
    role: Source,
    aliases: ["gltf", "glb", "import mesh", "load model", "File In SOP"],
    boundary_reason: IoBridge,
    extra_fields: {
        // (path, mesh_index, primitive_index, material_index, fit, recenter)
        // last parsed (or in flight). Any change re-triggers a background
        // parse — including fit/recenter, which apply to the freshly
        // parsed geometry ON that same background thread (D7), never
        // per-frame. fit/recenter are authoring-time enum/bool toggles,
        // not port-shadowed performance scalars, so a full re-parse on
        // change is the simple, correct choice over a second CPU-side
        // cache tier.
        last_key: (String, i32, i32, i32, u32, bool) =
            (String::new(), i32::MIN, i32::MIN, i32::MIN, u32::MAX, false),
        // Last successfully parsed geometry (CPU-side). Stays resident
        // across frames — only re-uploaded to `staging` when it changes.
        cached_verts: Vec<MeshVertex> = Vec::new(),
        // Shared-memory buffer holding `cached_verts`' bytes, copied into
        // the output buffer every frame via a blit.
        staging: Option<manifold_gpu::GpuBuffer> = None,
        staging_len_bytes: u64 = 0,
        // Background loader channel. `Some` means a parse is in flight;
        // we don't spawn another until it returns.
        pending_load: Option<mpsc::Receiver<Result<Vec<MeshVertex>, String>>> = None,
        // Whether `staging` currently reflects `cached_verts`.
        uploaded: bool = false,
        // Bumped every time a background parse lands (step 3) — a cheap
        // content-generation counter, distinct from `last_key`: `last_key`
        // updates the instant a new selection is requested, before the
        // async parse finishes, so gating the per-frame copy on it would
        // incorrectly skip while stale vertices are still in
        // `cached_verts`. RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1.
        content_version: u64 = 0,
        // (content_version, dst identity) the staging→output copy last
        // ran for. Sentinel values guarantee the first real copy runs.
        last_copied_content_version: u64 = u64::MAX,
        last_copied_dst_identity: usize = 0,
    },
}

impl Primitive for GltfMeshSource {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // 1. Params.
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let mesh_index = match ctx.params.get("mesh_index") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => -1,
        };
        let primitive_index = match ctx.params.get("primitive_index") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => -1,
        };
        let material_index = match ctx.params.get("material_index") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => -1,
        };
        let fit_idx = match ctx.params.get("fit") {
            Some(ParamValue::Enum(v)) => (*v).min((GLTF_FIT_MODES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => {
                f.round().clamp(0.0, (GLTF_FIT_MODES.len() - 1) as f32) as u32
            }
            _ => 0,
        };
        let recenter = matches!(ctx.params.get("recenter"), Some(ParamValue::Bool(true)));
        let fit_unit_box = fit_idx == 1;

        // 2. Re-trigger a background parse if the effective selection (or
        // the fit/recenter authoring choice) changed since the last one we
        // started.
        let key = (path.clone(), mesh_index, primitive_index, material_index, fit_idx, recenter);
        if key != self.last_key && self.pending_load.is_none() {
            self.last_key = key;
            self.cached_verts.clear();
            self.staging = None;
            self.staging_len_bytes = 0;
            self.uploaded = false;
            if !path.is_empty() {
                // material_index takes precedence: when set, it selects
                // every primitive of that material across the scene
                // (the importer's per-material object). Otherwise the
                // mesh_index / primitive_index path applies.
                let selector = if material_index == DEFAULT_MATERIAL_MESH_PARAM {
                    // GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171): the
                    // synthetic default-material object — every primitive
                    // with no glTF material, scene-wide.
                    GltfMeshSelector::DefaultMaterial
                } else if material_index >= 0 {
                    GltfMeshSelector::Material {
                        material_index: material_index as u32,
                    }
                } else if mesh_index < 0 {
                    GltfMeshSelector::WholeScene
                } else if primitive_index < 0 {
                    GltfMeshSelector::Mesh {
                        mesh_index: mesh_index as u32,
                    }
                } else {
                    GltfMeshSelector::Primitive {
                        mesh_index: mesh_index as u32,
                        primitive_index: primitive_index as u32,
                    }
                };
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = load_gltf_mesh(&path_buf, selector)
                        .map(|verts| apply_mesh_fit(verts, fit_unit_box, recenter));
                    let _ = tx.send(result);
                });
                self.pending_load = Some(rx);
            }
        }

        // 3. Drain any completed background parse.
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok(verts)) => {
                    self.cached_verts = verts;
                    self.uploaded = false;
                    self.content_version = self.content_version.wrapping_add(1);
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_mesh_source: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still in flight — put the receiver back.
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_mesh_source: background load channel disconnected");
                }
            }
        }

        // 4. Output buffer + capacity.
        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let capacity = dst.size / std::mem::size_of::<MeshVertex>() as u64;

        if self.cached_verts.is_empty() {
            // Nothing parsed yet (or the path is empty / failed) — leave
            // the pre-bound buffer's existing contents; downstream nodes
            // see whatever they last saw (or zeros on first run).
            return;
        }

        // 5. (Re)build the staging buffer when cached_verts changed.
        if !self.uploaded {
            let n = self.cached_verts.len().min(capacity as usize);
            if self.cached_verts.len() > capacity as usize {
                log::warn!(
                    "node.gltf_mesh_source: mesh has {} vertices, capacity {} — truncating",
                    self.cached_verts.len(),
                    capacity
                );
            }
            let bytes = &self.cached_verts[..n];
            let len_bytes = (n * std::mem::size_of::<MeshVertex>()) as u64;
            let device = ctx.gpu_encoder().device;
            let staging = device.create_buffer_shared(len_bytes.max(1));
            unsafe {
                staging.write(0, bytemuck::cast_slice(bytes));
            }
            self.staging = Some(staging);
            self.staging_len_bytes = len_bytes;
            self.uploaded = true;
        }

        // 6. Copy staging → dst, gated (RENDER_SCENE_PERF_OPTIMIZATION
        // P1/R1): skip when the cached content hasn't changed since the
        // last completed copy AND dst is the same physical buffer we
        // copied into last frame — pool recycling can hand back a
        // different physical buffer with stale bytes even when our own
        // cache is untouched, so identity gates alongside content (the
        // `last_mip_identity` precedent, extended to buffers via
        // `GpuBuffer::identity_key`).
        if let Some(staging) = &self.staging {
            let dst_identity = dst.identity_key();
            let unchanged = self.content_version == self.last_copied_content_version
                && dst_identity == self.last_copied_dst_identity;
            if unchanged {
                ctx.mark_outputs_unchanged();
            } else {
                let copy_size = self.staging_len_bytes.min(dst.size);
                if copy_size > 0 {
                    ctx.gpu_encoder()
                        .native_enc
                        .copy_buffer_to_buffer(staging, dst, copy_size);
                }
                self.last_copied_content_version = self.content_version;
                self.last_copied_dst_identity = dst_identity;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{ArrayType, PortType};

    #[test]
    fn gltf_mesh_source_declares_zero_inputs_and_mesh_array_output() {
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(GltfMeshSource::TYPE_ID, "node.gltf_mesh_source");
        assert!(GltfMeshSource::INPUTS.is_empty());
        assert_eq!(GltfMeshSource::OUTPUTS.len(), 1);
        assert_eq!(GltfMeshSource::OUTPUTS[0].name, "vertices");
        assert_eq!(GltfMeshSource::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn gltf_mesh_source_param_names_in_order() {
        let names: Vec<&str> = GltfMeshSource::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "path",
                "mesh_index",
                "primitive_index",
                "material_index",
                "max_capacity",
                "fit",
                "recenter",
                "source_vertex_count",
                "source_bbox_radius"
            ]
        );
    }

    #[test]
    fn fit_defaults_to_none_and_recenter_defaults_to_true() {
        let fit = GltfMeshSource::PARAMS.iter().find(|p| p.name == "fit").unwrap();
        assert_eq!(fit.ty, ParamType::Enum);
        assert_eq!(fit.default, ParamValue::Enum(0));
        assert_eq!(fit.enum_values, GLTF_FIT_MODES);
        assert_eq!(GLTF_FIT_MODES[0], "none");
        assert_eq!(GLTF_FIT_MODES[1], "unit_box");

        let recenter = GltfMeshSource::PARAMS.iter().find(|p| p.name == "recenter").unwrap();
        assert_eq!(recenter.ty, ParamType::Bool);
        assert_eq!(recenter.default, ParamValue::Bool(true));
    }

    #[test]
    fn primitive_registers() {
        let prim = GltfMeshSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_mesh_source");
    }

    fn mk_vertex(pos: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal: [0.3, 0.9, 0.1],
            _pad1: 0.0,
            uv: [0.25, 0.75],
            _pad2: [0.0, 0.0],
        }
    }

    /// An off-scale, off-center vertex set: bounding box spans
    /// x:[10,14], y:[100,102], z:[-5,-1] — center (12,101,-3), longest
    /// extent 4 (x and z axes tie; y is 2).
    fn off_scale_off_center_verts() -> Vec<MeshVertex> {
        vec![
            mk_vertex([10.0, 100.0, -5.0]),
            mk_vertex([14.0, 100.0, -5.0]),
            mk_vertex([10.0, 102.0, -1.0]),
            mk_vertex([14.0, 102.0, -1.0]),
        ]
    }

    #[test]
    fn fit_none_is_a_byte_identical_no_op() {
        // The old-preset-unaffected proof (D7, §3 gate): fit=none never
        // touches position/normal/uv, regardless of recenter.
        let verts = off_scale_off_center_verts();
        let out_a = apply_mesh_fit(verts.clone(), false, true);
        let out_b = apply_mesh_fit(verts.clone(), false, false);
        for (orig, fit) in verts.iter().zip(out_a.iter()) {
            assert_eq!(orig.position, fit.position);
            assert_eq!(orig.normal, fit.normal);
            assert_eq!(orig.uv, fit.uv);
        }
        for (orig, fit) in verts.iter().zip(out_b.iter()) {
            assert_eq!(orig.position, fit.position);
        }
    }

    #[test]
    fn fit_unit_box_recenters_and_bounds_the_longest_axis_to_one() {
        let verts = off_scale_off_center_verts();
        let out = apply_mesh_fit(verts, true, true);

        let mut min = out[0].position;
        let mut max = out[0].position;
        for v in &out {
            for axis in 0..3 {
                min[axis] = min[axis].min(v.position[axis]);
                max[axis] = max[axis].max(v.position[axis]);
            }
        }
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        for (axis, c) in center.iter().enumerate() {
            assert!(
                c.abs() < 1e-5,
                "axis {axis} center {c} should be ~0 (recenter=true)"
            );
        }
        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let longest = size[0].max(size[1]).max(size[2]);
        assert!(
            (longest - 1.0).abs() < 1e-5,
            "longest bbox axis should be exactly 1.0, got {longest}"
        );
        // Normals/uv pass through untouched.
        for v in &out {
            assert_eq!(v.normal, [0.3, 0.9, 0.1]);
        }
    }

    #[test]
    fn fit_unit_box_without_recenter_keeps_original_center() {
        let verts = off_scale_off_center_verts();
        let out = apply_mesh_fit(verts, true, false);

        let mut min = out[0].position;
        let mut max = out[0].position;
        for v in &out {
            for axis in 0..3 {
                min[axis] = min[axis].min(v.position[axis]);
                max[axis] = max[axis].max(v.position[axis]);
            }
        }
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        // Original center was (12, 101, -3); recenter=false keeps it there.
        assert!((center[0] - 12.0).abs() < 1e-4);
        assert!((center[1] - 101.0).abs() < 1e-4);
        assert!((center[2] - (-3.0)).abs() < 1e-4);
    }
}

/// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 gate. Run deliberately:
/// `cargo test -p manifold-renderer --features gpu-proofs
/// node_graph::primitives::gltf_mesh_source::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::effect_node::ParamValues;
    use crate::node_graph::{FrameTime, MetalBackend};
    use crate::TestDevice;
    use manifold_core::{Beats, Seconds};

    const CAPACITY: u32 = 20_000;

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    fn helmet_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/DamagedHelmet.glb")
    }

    fn params_at(path: &str, mesh_index: f32, capacity: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(Cow::Borrowed("path"), ParamValue::String(path.to_string().into()));
        p.insert(Cow::Borrowed("mesh_index"), ParamValue::Float(mesh_index));
        p.insert(Cow::Borrowed("primitive_index"), ParamValue::Float(-1.0));
        p.insert(Cow::Borrowed("material_index"), ParamValue::Float(-1.0));
        p.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(capacity));
        p.insert(Cow::Borrowed("fit"), ParamValue::Enum(0));
        p.insert(Cow::Borrowed("recenter"), ParamValue::Bool(true));
        p
    }

    /// Run one frame directly against a real GPU backend (no Graph/Executor
    /// needed — this Source primitive has zero inputs). Returns whether
    /// `mark_outputs_unchanged` was declared this frame.
    fn run_once(
        prim: &mut GltfMeshSource,
        backend: &MetalBackend,
        device: &TestDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
        time: FrameTime,
    ) -> bool {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(&[], backend_ref, &[]);
        let outputs = NodeOutputs::new(
            output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
        );
        let mut native_enc = device.create_encoder("gltf-mesh-source-test");
        let unchanged;
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
            let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, Some(&mut gpu));
            prim.run(&mut ctx);
            unchanged = ctx.outputs_unchanged;
        }
        native_enc.commit_and_wait_completed();
        unchanged
    }

    fn readback(backend: &MetalBackend, slot: Slot) -> Vec<u8> {
        let buf = backend.array_buffer(slot).expect("array buffer retained");
        let ptr = buf.mapped_ptr().expect("shared buffer");
        unsafe { std::slice::from_raw_parts(ptr, buf.size() as usize) }.to_vec()
    }

    fn settle(
        prim: &mut GltfMeshSource,
        backend: &MetalBackend,
        device: &TestDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
    ) {
        for _ in 0..200 {
            run_once(prim, backend, device, output_scratch, params, frame_time());
            if !prim.cached_verts.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("gltf_mesh_source: parse never settled");
    }

    fn make_buffer_backend(device: &TestDevice) -> (MetalBackend, ResourceId, Slot) {
        let mut backend = MetalBackend::new(device.arc(), 64, 64, manifold_gpu::GpuTextureFormat::Rgba8Unorm);
        let r_out = ResourceId(0);
        let buf =
            device.create_buffer_shared((CAPACITY as u64) * std::mem::size_of::<MeshVertex>() as u64);
        let slot = backend.pre_bind_array(r_out, buf);
        (backend, r_out, slot)
    }

    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 gate: on a static
    /// asset, frame 2's output is bit-identical to frame 1's, and the
    /// staging→output copy skip (`mark_outputs_unchanged`) fires on frame 2.
    #[test]
    fn frame2_matches_frame1_on_static_asset_and_declares_unchanged() {
        let path = helmet_fixture_path();
        if !path.exists() {
            println!("frame2_matches_frame1_on_static_asset_and_declares_unchanged: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();
        let (backend, _r_out, slot) = make_buffer_backend(&device);
        let scratch: Vec<(&'static str, Slot)> = vec![("vertices", slot)];

        let params = params_at(path.to_str().unwrap(), -1.0, CAPACITY as f32);
        let mut prim = GltfMeshSource::new();
        settle(&mut prim, &backend, &device, &scratch, &params);
        let frame1 = readback(&backend, slot);

        let unchanged = run_once(&mut prim, &backend, &device, &scratch, &params, frame_time());
        assert!(unchanged, "settled static frame must declare mark_outputs_unchanged");
        let frame2 = readback(&backend, slot);
        assert_eq!(frame1, frame2, "frame 2 must be bit-identical to frame 1 on a static asset");
    }

    /// A content-affecting param change (`fit` toggled on) must NOT be
    /// skipped, and must match a FRESH executor built with that param baked
    /// in from the start.
    #[test]
    fn fit_param_change_matches_fresh_executor() {
        let path = helmet_fixture_path();
        if !path.exists() {
            println!("fit_param_change_matches_fresh_executor: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();

        let (backend_a, _r_out_a, slot_a) = make_buffer_backend(&device);
        let scratch_a: Vec<(&'static str, Slot)> = vec![("vertices", slot_a)];
        let params_none = params_at(path.to_str().unwrap(), -1.0, CAPACITY as f32);
        let mut prim_a = GltfMeshSource::new();
        settle(&mut prim_a, &backend_a, &device, &scratch_a, &params_none);

        let mut params_fit = params_none.clone();
        params_fit.insert(Cow::Borrowed("fit"), ParamValue::Enum(1));
        // Re-triggers a background parse (fit is part of `last_key`) — poll
        // until it lands, same as the initial settle.
        for _ in 0..200 {
            let unchanged = run_once(&mut prim_a, &backend_a, &device, &scratch_a, &params_fit, frame_time());
            if prim_a.pending_load.is_none() && prim_a.last_key.4 == 1 {
                assert!(!unchanged, "a content-changing param must not gate as unchanged");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let a_output = readback(&backend_a, slot_a);

        let (backend_b, _r_out_b, slot_b) = make_buffer_backend(&device);
        let scratch_b: Vec<(&'static str, Slot)> = vec![("vertices", slot_b)];
        let mut prim_b = GltfMeshSource::new();
        settle(&mut prim_b, &backend_b, &device, &scratch_b, &params_fit);
        let b_output = readback(&backend_b, slot_b);

        assert_eq!(
            a_output, b_output,
            "fit param change on a live gated executor must match a fresh executor built with that param"
        );
    }
}
