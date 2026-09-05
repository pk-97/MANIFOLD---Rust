//! `node.reflect_array` — reflect a whole `Array<InstanceTransform>` across
//! an axis-aligned plane, one mirrored copy per live source slot
//! (SCENE_MIRROR design D3/D4/D5). The scene mirror is an instance-array
//! transform, not a render pass: the reflected copies render through the
//! same whole-buffer instance draw as the originals, so they are shaded by
//! the same lights and compose with the Scene Loop for free (the loop's
//! copies arrive as instances).
//!
//! Output capacity is FIXED at 2x the input capacity at plan
//! pre-allocation — `axis` / `plane_offset` / `enabled` are live card
//! writes and never trigger a rebuild (the BUG-757c capacity rule, applied
//! up front). Slots [0, cap) are the originals, masked to zero-scale where
//! the source slot is dead; slots [cap, 2cap) are the mirrored copies,
//! existing iff the source slot has nonzero scale and the gate is on.
//! The D11 draw reads the whole buffer and zero-scale slots rasterize
//! nothing.
//!
//! `rot_pad.w` carries the mirror marker in-band (D5): 0 = original slot,
//! plane component + 1 (1 = x, 2 = y, 3 = z) = mirrored slot. The plane is
//! the same for +axis and -axis (`M = I - 2 a a^T` is sign-blind), so the
//! marker encodes the plane component, not the signed axis;
//! render_scene.wgsl's vertex stage flips that component of the vertex
//! position and normal before the stored rotation, yielding the exact
//! mirrored point and normal (`R' M = M R`).
//!
//! The `in` port is optional: unwired means one identity instance at the
//! origin (the scene_array count convention). The body reads `buf_in` as a
//! BufferGather input (slot `idx % cap` — a coincident pre-read would run
//! off the end of the smaller input array), which makes the atom a fusion
//! boundary today, exactly like `node.neighbor_smooth`: the fused buffer
//! wrapper keys its dispatch on the input array length and cannot yet
//! express a 2x-capacity output. Tracked compiler debt, not an exemption.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout. Params in PARAMS order:
/// axis (Enum→u32), plane_offset (f32), enabled (f32), then the
/// codegen-injected dispatch_count (= the OUTPUT capacity = 2x input
/// capacity). 4 words = 16 bytes, no pad.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ReflectUniforms {
    axis: u32,
    plane_offset: f32,
    enabled: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ReflectArray,
    type_id: "node.reflect_array",
    purpose: "Reflect an Array<InstanceTransform> across an axis-aligned plane: slots [0, cap) pass the input through (live slots only), slots [cap, 2cap) are the planar reflections (live sources only, gate on). pos' = M(pos - d a) + d a with M = I - 2 a a^T; rotation stored as the proper conjugation R' = M R M (sign map on the Euler triple); scale kept positive (the mirror flip rides on the vertex in render_scene.wgsl, not on the scale); rot_pad.w carries the mirror marker — 0 = original, k > 0 = mirrored across the plane perpendicular to component k-1. Output capacity is fixed at 2x the input capacity so axis/plane_offset/enabled are live writes, never a rebuild. The scene-mirror atom (SCENE_MIRROR D3/D4/D5) — pair upstream with node.scene_array; unwired in = one identity instance.",
    inputs: {
        in: Array(InstanceTransform) optional,
    },
    outputs: {
        out: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // +Y (floor)
            range: None,
            enum_values: super::scene_array::AXIS_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("plane_offset"),
            label: "Plane Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("enabled"),
            label: "Enabled",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is fixed at 2x the input capacity (D4): the output buffer is sized at plan pre-allocation and axis/plane_offset/enabled writes apply in place — the BUG-757c rule. Liveness is read in-band: a source slot with pos_scale.w == 0 (scene_array's surplus mask) gets no mirrored copy and its original slot is zeroed (INV-MR5). enabled == 0 zeroes only the mirrored half — originals pass through byte-identical (INV-MR1, off is free). Marker contract (INV-MR3, P1-amended): rot_pad.w in {0,1,2,3} — 0 = original slot, k > 0 = mirrored across the plane perpendicular to component k-1; only this atom writes nonzero, and render_scene.wgsl's vertex stage is the sole consumer. Mirrored scale stays positive: the flip rides on the vertex in the shader (exactness R'(w M v) + t' = M R (w v) + t'), a negated uniform scale would be a central inversion, not a reflection. BufferGather body: the atom is a standalone-only fusion boundary until the fused buffer codegen can express a non-coincident output capacity (tracked as scene-mirror-blocked-* beads, not exempted). Unwired in = one identity instance at the origin.",
    examples: [],
    picker: { label: "Reflect Array", category: Atom },
    summary: "Makes a mirrored copy of every instance across a plane — drop a reflected scene under the floor and ride the offset.",
    category: Geometry3D,
    role: Filter,
    aliases: ["reflect array", "mirror array", "instance mirror", "reflection"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/reflect_array_body.wgsl"),
    input_access: [BufferGather],
    extra_fields: {
        identity_fallback: Option<manifold_gpu::GpuBuffer> = None,
    },
}

impl Primitive for ReflectArray {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        // D4: fixed at 2x the INPUT capacity, never a function of the live
        // param values — the output buffer is allocated once at plan
        // pre-allocation and every live card write applies in place.
        // Unwired `in` is the one-identity-instance convention, so the
        // capacity is 2 x 1.
        let in_cap = input_capacities
            .iter()
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n)
            .unwrap_or(1);
        Some(in_cap * 2)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 2, // +Y
        };
        let plane_offset = ctx.scalar_or_param("plane_offset", 0.0);
        let enabled = ctx.scalar_or_param("enabled", 1.0);

        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let out_cap = (out_buf.size / item_size) as u32;
        if out_cap == 0 || !out_cap.is_multiple_of(2) {
            return;
        }

        // The kernel derives the input capacity from arrayLength(buf_in);
        // the planner contract (array_output_capacity) is out = 2 x in. A
        // mismatch is a planner bug — writing with wrap-around indices
        // would render garbage, so refuse to dispatch.
        let in_buf = ctx.inputs.array("in");
        if let Some(in_buf) = in_buf
            && (in_buf.size / item_size) * 2 != out_buf.size / item_size
        {
            log::warn!(
                "node.reflect_array: input capacity {} does not match half the \
                 output capacity {} — planner contract violated, skipping",
                in_buf.size / item_size,
                out_buf.size / item_size,
            );
            return;
        }

        let gpu = ctx.gpu_encoder();
        if self.pipeline.is_none() {
            // Single-source: kernel generated from the wgsl_body (buffer
            // standalone codegen). Bindings match: uniform(0), buf_in(1),
            // buf_out(2).
            self.pipeline = Some(gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.reflect_array standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.reflect_array",
            ));
        }

        // `in` is optional: unwired means one identity instance at the
        // origin (D3). The kernel contract reads arrayLength(buf_in), so
        // bind a cached single-element identity buffer. Shared buffers are
        // CPU-mapped — write the identity once at creation.
        if in_buf.is_none() && self.identity_fallback.is_none() {
            let buf = gpu.device.create_buffer_shared(item_size);
            let ident = InstanceTransform {
                pos_scale: [0.0, 0.0, 0.0, 1.0],
                rot_pad: [0.0; 4],
            };
            let ptr = buf
                .mapped_ptr()
                .expect("shared identity fallback buffer") as *mut InstanceTransform;
            unsafe { ptr.write(ident) };
            self.identity_fallback = Some(buf);
        }

        let uniforms = ReflectUniforms {
            axis,
            plane_offset,
            enabled,
            dispatch_count: out_cap,
        };

        let pipeline = self.pipeline.as_ref().expect("pipeline initialized above");
        let bound_in: &manifold_gpu::GpuBuffer = match in_buf {
            Some(b) => b,
            None => self
                .identity_fallback
                .as_ref()
                .expect("identity fallback initialized above"),
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: bound_in,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [out_cap.div_ceil(256), 1, 1],
            "node.reflect_array",
        );
    }
}

/// The conjugated-Euler closed form, pinned: render_scene.wgsl builds
/// `R = Rz(z) * Ry(y) * Rx(x)` (column vectors, x-angle applied first).
/// For a reflection `M = diag(m)` the conjugation `R' = M R M` maps the
/// Euler triple by `θx' = m_y m_z θx`, `θy' = m_x m_z θy`,
/// `θz' = m_x m_y θz` — with an axis-aligned plane at component c
/// (m_c = -1) the angle about the mirror normal keeps its sign and the
/// two about in-plane axes negate. `m_mirror`/`euler_xyz` below are the
/// CPU port of that shader math; if the shader's multiplication order
/// ever changes, the tests below are the tripwire.
#[cfg(test)]
fn euler_xyz(angles: [f32; 3]) -> [[f32; 3]; 3] {
    let (x, y, z) = (angles[0], angles[1], angles[2]);
    let (cx, sx, cy, sy, cz, sz) = (x.cos(), x.sin(), y.cos(), y.sin(), z.cos(), z.sin());
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mat_mul(&mat_mul(&rz, &ry), &rx)
}

/// Axis-aligned plane reflection matrix: `M = I - 2 a a^T` for the
/// unit axis behind the 6-value enum (0=+X .. 5=-Z). Sign-blind:
/// +axis and -axis give the same matrix.
#[cfg(test)]
fn m_mirror(axis: u32) -> [[f32; 3]; 3] {
    let c = (axis / 2) as usize;
    let mut m = [[0.0f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        row[i] = if i == c { -1.0 } else { 1.0 };
    }
    m
}

#[cfg(test)]
#[cfg(test)]
fn mat_mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

#[cfg(test)]
fn mat_vec(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// CPU port of the body's Euler sign map: the angle about the mirror
/// normal keeps its sign, the two about in-plane axes negate.
#[cfg(test)]
fn conjugate_euler(angles: [f32; 3], axis: u32) -> [f32; 3] {
    match axis / 2 {
        0 => [angles[0], -angles[1], -angles[2]],
        1 => [-angles[0], angles[1], -angles[2]],
        _ => [-angles[0], -angles[1], angles[2]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;


    #[test]
    fn euler_conjugation_matches_mrm_for_all_six_axes() {
        let triples = [
            [0.3f32, -1.2, 2.2],
            [-2.7, 0.05, 1.1],
            [1.9, 2.8, -0.4],
            [0.0, 0.0, 0.0],
        ];
        for axis in 0u32..6 {
            let m = m_mirror(axis);
            for t in triples {
                let r = euler_xyz(t);
                let mrm = mat_mul(&mat_mul(&m, &r), &m);
                let conj = euler_xyz(conjugate_euler(t, axis));
                for i in 0..3 {
                    for j in 0..3 {
                        assert!(
                            (mrm[i][j] - conj[i][j]).abs() < 1e-5,
                            "axis {axis} angles {t:?}: M R M [{i}][{j}] = {} but \
                             conjugated Euler gives {}",
                            mrm[i][j],
                            conj[i][j]
                        );
                    }
                }
                // The conjugated rotation must be PROPER (det +1) — Euler-
                // representable, unlike M R itself.
                let det = conj[0][0] * (conj[1][1] * conj[2][2] - conj[1][2] * conj[2][1])
                    - conj[0][1] * (conj[1][0] * conj[2][2] - conj[1][2] * conj[2][0])
                    + conj[0][2] * (conj[1][0] * conj[2][1] - conj[1][1] * conj[2][0]);
                assert!((det - 1.0).abs() < 1e-4, "det(R') = {det}, want +1");
            }
        }
    }

    #[test]
    fn mirrored_world_transform_is_exact_planar_reflection() {
        // The reason the vertex shader flips the vertex component BEFORE
        // the stored rotation: for a probe vertex v the stored TRS must
        // reproduce the true mirror image
        //   R'(w M v) + p'  ==  M(R(w v) + p - d a) + d a
        let angles = [0.7f32, -1.3, 2.1];
        let pos = [1.5f32, -2.0, 3.5];
        let w = 0.8f32;
        let v = [0.4f32, -0.9, 1.2];
        for axis in 0u32..6 {
            let d = 2.25f32;
            let m = m_mirror(axis);
            let c = (axis / 2) as usize;
            let s = if axis % 2 == 1 { -1.0f32 } else { 1.0 };

            // Stored transform (what the body writes).
            let r_prime = euler_xyz(conjugate_euler(angles, axis));
            let mut p_prime = pos;
            p_prime[c] = -pos[c] + 2.0 * d * s;

            // Vertex flip before rotation.
            let mut mv = v;
            mv[c] = -mv[c];
            let stored = {
                let rv = mat_vec(&r_prime, [mv[0] * w, mv[1] * w, mv[2] * w]);
                [rv[0] + p_prime[0], rv[1] + p_prime[1], rv[2] + p_prime[2]]
            };

            // True planar reflection of the world-space point.
            let r = euler_xyz(angles);
            let world = {
                let rv = mat_vec(&r, [v[0] * w, v[1] * w, v[2] * w]);
                [rv[0] + pos[0], rv[1] + pos[1], rv[2] + pos[2]]
            };
            let mut a = [0.0f32; 3];
            a[c] = s * d; // plane point d a
            let true_mirror = {
                let rel = [
                    world[0] - a[0],
                    world[1] - a[1],
                    world[2] - a[2],
                ];
                let refl = mat_vec(&m, rel);
                [refl[0] + a[0], refl[1] + a[1], refl[2] + a[2]]
            };

            for i in 0..3 {
                assert!(
                    (stored[i] - true_mirror[i]).abs() < 1e-4,
                    "axis {axis}: stored {stored:?} != true mirror {true_mirror:?}"
                );
            }
        }
    }

    #[test]
    fn reflect_array_declares_optional_array_in_and_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(ReflectArray::TYPE_ID, "node.reflect_array");

        let in_port = ReflectArray::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(!in_port.required, "unwired in = one identity instance (D3)");
        assert_eq!(in_port.ty, PortType::Array(layout));

        assert_eq!(ReflectArray::OUTPUTS.len(), 1);
        assert_eq!(ReflectArray::OUTPUTS[0].name, "out");
        assert_eq!(ReflectArray::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn reflect_array_has_axis_offset_enabled_params() {
        let names: Vec<&str> = ReflectArray::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["axis", "plane_offset", "enabled"]);

        let axis = ReflectArray::PARAMS.iter().find(|p| p.name == "axis").unwrap();
        assert_eq!(axis.ty, ParamType::Enum);
        assert_eq!(axis.enum_values, super::super::scene_array::AXIS_LABELS);
        assert_eq!(axis.enum_values.len(), 6);

        let enabled = ReflectArray::PARAMS.iter().find(|p| p.name == "enabled").unwrap();
        assert_eq!(enabled.default, ParamValue::Float(1.0));
    }

    /// INV-MR4, the capacity layer: the output buffer is sized for 2x the
    /// input capacity, whatever the live param values — the mirror's
    /// axis/offset/enabled writes are live card writes, never a rebuild
    /// (the BUG-757c rule applied up front).
    #[test]
    fn output_capacity_is_twice_input_capacity_not_param_dependent() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ReflectArray::new();
        let params = ParamValues::default();

        for caps in [("in", 8_u32), ("in", 1_u32), ("in", 160_000_u32)] {
            assert_eq!(
                Primitive::array_output_capacity(&prim, "out", &params, &[caps]),
                Some(caps.1 * 2),
                "capacity must follow the input capacity, not any live value"
            );
        }

        // Unwired `in`: the one-identity-instance convention → 2 x 1.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &[]),
            Some(2),
        );

        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None,
            "a nonexistent port carries no capacity"
        );
    }

    /// The gather body is a per-element map (Pointwise) that indexes its
    /// input array itself; the fused buffer wrapper keys dispatch on the
    /// input array length and cannot express the 2x-capacity output, so
    /// the region grower must keep this atom OUT of fused regions — a
    /// fused reflect would dispatch input-capacity threads and leave the
    /// mirrored half of the output buffer unwritten (garbage transforms on
    /// screen). This is the standing debt's safety half; the fused
    /// numerical proof becomes mandatory once the codegen can express the
    /// capacity (tracked with the atom's composition notes).
    #[test]
    fn reflect_array_never_enters_a_fused_region() {
        use crate::node_graph::freeze::region::partition_regions;
        use crate::node_graph::persistence::PrimitiveRegistry;
        use manifold_core::effect_graph_def::EffectGraphDef;

        // scene_array → reflect_array → rotation_jitter: the jitter is a
        // coincident per-element atom that WOULD fuse with a fusable
        // reflect. If reflect_array ever slips into a region, the mirrored
        // half goes unwritten — fail loud here instead.
        let json = r#"{
            "version": 1, "name": "mirror", "nodes": [
                { "id": 0, "typeId": "node.scene_array", "nodeId": "arr" },
                { "id": 1, "typeId": "node.reflect_array", "nodeId": "refl" },
                { "id": 2, "typeId": "node.rotation_jitter", "nodeId": "jitter" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "instances" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &PrimitiveRegistry::with_builtin());
        for r in &regions {
            for m in &r.members {
                assert_ne!(
                    m.doc_id, 1,
                    "reflect_array must not fuse: a fused buffer region \
                     dispatches input-capacity threads and would leave the \
                     mirrored half of the 2x output buffer unwritten"
                );
            }
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ReflectArray::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.reflect_array");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_gpu::GpuDevice;

    /// CPU oracle — bit-for-bit the reflect_array_body.wgsl semantics.
    fn cpu_reflect(
        input: &[InstanceTransform],
        axis: u32,
        plane_offset: f32,
        enabled: f32,
    ) -> Vec<InstanceTransform> {
        let cap = input.len();
        let zero = InstanceTransform {
            pos_scale: [0.0; 4],
            rot_pad: [0.0; 4],
        };
        let mut out = vec![zero; cap * 2];
        for (j, src) in input.iter().enumerate() {
            let live = src.pos_scale[3] != 0.0;
            out[j] = if live {
                InstanceTransform {
                    pos_scale: src.pos_scale,
                    rot_pad: [src.rot_pad[0], src.rot_pad[1], src.rot_pad[2], 0.0],
                }
            } else {
                zero
            };
            if !live || enabled == 0.0 {
                continue;
            }
            let comp = (axis / 2) as usize;
            let s = if axis % 2 == 1 { -1.0f32 } else { 1.0 };
            let mut p = [src.pos_scale[0], src.pos_scale[1], src.pos_scale[2]];
            p[comp] = -p[comp] + 2.0 * plane_offset * s;
            let r = src.rot_pad;
            let r2 = match comp {
                0 => [r[0], -r[1], -r[2]],
                1 => [-r[0], r[1], -r[2]],
                _ => [-r[0], -r[1], r[2]],
            };
            out[cap + j] = InstanceTransform {
                pos_scale: [p[0], p[1], p[2], src.pos_scale[3]],
                rot_pad: [r2[0], r2[1], r2[2], comp as f32 + 1.0],
            };
        }
        out
    }

    fn upload(device: &GpuDevice, items: &[InstanceTransform]) -> manifold_gpu::GpuBuffer {
        let buf = device.create_buffer_shared(items.len() as u64 * 32);
        let ptr = buf.mapped_ptr().expect("shared input buffer") as *mut InstanceTransform;
        unsafe { std::ptr::copy_nonoverlapping(items.as_ptr(), ptr, items.len()) };
        buf
    }

    fn read_back(buf: &manifold_gpu::GpuBuffer, capacity: u32) -> Vec<InstanceTransform> {
        let ptr = buf.mapped_ptr().expect("shared output buffer") as *const InstanceTransform;
        unsafe { std::slice::from_raw_parts(ptr, capacity as usize) }.to_vec()
    }

    fn dispatch(
        device: &GpuDevice,
        pipeline: &manifold_gpu::GpuComputePipeline,
        in_buf: &manifold_gpu::GpuBuffer,
        out_buf: &manifold_gpu::GpuBuffer,
        out_cap: u32,
        axis: u32,
        plane_offset: f32,
        enabled: f32,
    ) {
        let uniforms = ReflectUniforms {
            axis,
            plane_offset,
            enabled,
            dispatch_count: out_cap,
        };
        let mut enc = device.create_encoder("reflect_array_test");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out_buf, offset: 0 },
            ],
            [out_cap.div_ceil(256), 1, 1],
            "reflect_array_test",
        );
        enc.commit_and_wait_completed();
    }

    fn assert_matches_cpu(
        gpu_data: &[InstanceTransform],
        expected: &[InstanceTransform],
        ctx: &str,
    ) {
        assert_eq!(gpu_data.len(), expected.len(), "{ctx}: length");
        for (i, (g, e)) in gpu_data.iter().zip(expected.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (g.pos_scale[c] - e.pos_scale[c]).abs() < 1e-6,
                    "{ctx} slot {i} pos_scale[{c}]: gpu={} expected={}",
                    g.pos_scale[c],
                    e.pos_scale[c]
                );
                assert!(
                    (g.rot_pad[c] - e.rot_pad[c]).abs() < 1e-6,
                    "{ctx} slot {i} rot_pad[{c}]: gpu={} expected={}",
                    g.rot_pad[c],
                    e.rot_pad[c]
                );
            }
        }
    }

    fn live_instance(pos: [f32; 3], scale: f32, rot: [f32; 3]) -> InstanceTransform {
        InstanceTransform {
            pos_scale: [pos[0], pos[1], pos[2], scale],
            rot_pad: [rot[0], rot[1], rot[2], 0.0],
        }
    }

    /// INV-MR2: every live mirrored slot is the exact planar reflection of
    /// its source — all six axes, nonzero offset, nontrivial rotation —
    /// against the CPU oracle (which the default-suite matrix tests pin to
    /// the true planar reflection).
    #[test]
    fn exact_reflection_matches_cpu_all_six_axes() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        let input = [
            live_instance([1.0, 2.0, -3.0], 1.5, [0.4, -0.8, 1.7]),
            live_instance([-4.0, 0.5, 2.5], 0.7, [-1.9, 0.3, 0.6]),
            live_instance([0.0, -3.0, 1.0], 2.0, [2.4, 1.1, -0.5]),
        ];
        let in_buf = upload(&device, &input);
        let out_cap = (input.len() * 2) as u32;
        let out_buf = device.create_buffer_shared(out_cap as u64 * 32);

        for axis in 0u32..6u32 {
            dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, axis, 2.25, 1.0);
            let gpu_data = read_back(&out_buf, out_cap);
            let expected = cpu_reflect(&input, axis, 2.25, 1.0);
            assert_matches_cpu(&gpu_data, &expected, "axis {axis}");
        }
    }

    /// INV-MR1 (off is free): enabled == 0 zeroes ONLY the mirrored half —
    /// the originals pass through byte-identical (positions, rotations,
    /// marker 0). Off must never delete the scene.
    #[test]
    fn identity_at_off_passes_originals_through() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        let input = [
            live_instance([1.0, 2.0, -3.0], 1.5, [0.4, -0.8, 1.7]),
            live_instance([-4.0, 0.5, 2.5], 0.7, [-1.9, 0.3, 0.6]),
        ];
        let in_buf = upload(&device, &input);
        let out_cap = (input.len() * 2) as u32;
        let out_buf = device.create_buffer_shared(out_cap as u64 * 32);

        dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, 2, 1.5, 0.0);
        let gpu_data = read_back(&out_buf, out_cap);

        let expected = cpu_reflect(&input, 2, 1.5, 0.0);
        assert_matches_cpu(&gpu_data, &expected, "enabled = 0");
        // Byte-identical originals, spelled out: same positions, same
        // rotations, marker 0, and the mirrored half fully zeroed.
        for (i, t) in gpu_data[..input.len()].iter().enumerate() {
            assert_eq!(
                t.pos_scale, input[i].pos_scale,
                "original slot {i} must pass through at off"
            );
            assert_eq!(
                t.rot_pad,
                [input[i].rot_pad[0], input[i].rot_pad[1], input[i].rot_pad[2], 0.0],
                "original slot {i} rotation must pass through, marker 0"
            );
        }
        for (i, t) in gpu_data[input.len()..].iter().enumerate() {
            assert_eq!(t.pos_scale, [0.0; 4], "mirrored slot {i} must be zero at off");
            assert_eq!(t.rot_pad, [0.0; 4], "mirrored slot {i} must be zero at off");
        }
    }

    /// INV-MR3: rot_pad.w is 0 on every original slot and exactly
    /// plane_component + 1 on every live mirrored slot — never the signed
    /// axis value (the marker encodes the plane, which is sign-blind).
    #[test]
    fn marker_discipline_plane_component_plus_one() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        let input = [
            live_instance([1.0, 2.0, -3.0], 1.5, [0.4, -0.8, 1.7]),
            live_instance([-4.0, 0.5, 2.5], 0.7, [-1.9, 0.3, 0.6]),
        ];
        let in_buf = upload(&device, &input);
        let out_cap = (input.len() * 2) as u32;
        let out_buf = device.create_buffer_shared(out_cap as u64 * 32);

        for axis in 0u32..6u32 {
            dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, axis, 0.75, 1.0);
            let gpu_data = read_back(&out_buf, out_cap);
            let want = (axis / 2) as f32 + 1.0;
            for (i, t) in gpu_data[..input.len()].iter().enumerate() {
                assert_eq!(t.rot_pad[3], 0.0, "original slot {i} marker must be 0");
            }
            for (i, t) in gpu_data[input.len()..].iter().enumerate() {
                assert_eq!(
                    t.rot_pad[3], want,
                    "mirrored slot {i} marker must be the plane component + 1 \
                     (axis {axis})"
                );
            }
        }
    }

    /// INV-MR4: the output buffer is fixed at 2x input capacity and live
    /// param writes apply in place — the same buffer, re-dispatched with
    /// new plane_offset/enabled values and NO rebuild, must reflect the new
    /// values (the BUG-757c test shape, applied up front).
    #[test]
    fn live_writes_without_rebuild() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        let input = [live_instance([1.0, 2.0, -3.0], 1.5, [0.4, -0.8, 1.7])];
        let in_buf = upload(&device, &input);
        let out_cap = (input.len() * 2) as u32;
        let out_buf = device.create_buffer_shared(out_cap as u64 * 32);

        // Gate on, offset 1.0.
        dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, 2, 1.0, 1.0);
        let gpu_data = read_back(&out_buf, out_cap);
        assert_matches_cpu(&gpu_data, &cpu_reflect(&input, 2, 1.0, 1.0), "offset 1 on");

        // Live writes, same buffers: offset moves, gate flips off, back on
        // at a new offset. Nothing is reallocated between dispatches.
        dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, 2, 4.0, 1.0);
        let gpu_data = read_back(&out_buf, out_cap);
        assert_matches_cpu(&gpu_data, &cpu_reflect(&input, 2, 4.0, 1.0), "offset 4 on");

        dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, 2, 4.0, 0.0);
        let gpu_data = read_back(&out_buf, out_cap);
        assert_matches_cpu(&gpu_data, &cpu_reflect(&input, 2, 4.0, 0.0), "offset 4 off");

        dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, 4, -2.0, 1.0);
        let gpu_data = read_back(&out_buf, out_cap);
        assert_matches_cpu(&gpu_data, &cpu_reflect(&input, 4, -2.0, 1.0), "axis +Z off -2");
    }

    /// INV-MR5: a mirrored copy exists iff its source slot has nonzero
    /// scale — a partially masked input (scene_array's surplus-mask shape)
    /// keeps live originals and mirrors, and dead slots contribute nothing
    /// on either half.
    #[test]
    fn liveness_by_scale_partially_masked_input() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        let dead = InstanceTransform {
            pos_scale: [9.0, 9.0, 9.0, 0.0], // zero scale = dead (INV-MR5)
            rot_pad: [1.0, 1.0, 1.0, 0.0],
        };
        let input = [
            live_instance([1.0, 2.0, -3.0], 1.5, [0.4, -0.8, 1.7]),
            dead,
            live_instance([-4.0, 0.5, 2.5], 0.7, [-1.9, 0.3, 0.6]),
            dead,
            live_instance([0.0, -3.0, 1.0], 2.0, [2.4, 1.1, -0.5]),
        ];
        let in_buf = upload(&device, &input);
        let out_cap = (input.len() * 2) as u32;
        let out_buf = device.create_buffer_shared(out_cap as u64 * 32);

        dispatch(&device, &pipeline, &in_buf, &out_buf, out_cap, 3, 1.25, 1.0);
        let gpu_data = read_back(&out_buf, out_cap);
        assert_matches_cpu(&gpu_data, &cpu_reflect(&input, 3, 1.25, 1.0), "masked");

        // Spelled out: dead slots are zero on BOTH halves even though the
        // dead source carried a nonzero position and rotation.
        for j in [1_usize, 3] {
            assert_eq!(gpu_data[j].pos_scale, [0.0; 4], "dead original {j} zeroed");
            assert_eq!(gpu_data[j].rot_pad, [0.0; 4], "dead original {j} zeroed");
            assert_eq!(
                gpu_data[input.len() + j].pos_scale,
                [0.0; 4],
                "dead source produced no mirrored copy"
            );
            assert_eq!(
                gpu_data[input.len() + j].rot_pad,
                [0.0; 4],
                "dead source produced no mirrored copy"
            );
        }
        assert!(
            gpu_data[input.len()].pos_scale[3] != 0.0,
            "live source must produce a mirrored copy"
        );
    }

    /// The flip-before exactness pin, on the GPU: a rotated (non-identity
    /// R) instance mirrored across a floor plane (+Y), with the expected
    /// world-space transform computed from the TRUE planar reflection
    /// M(R(w v) + t - d a) + d a — not from the atom's own oracle. This is
    /// the exact counterexample to the rejected flip-after / negated-scale
    /// construction (a negated uniform scale is a central inversion: it
    /// lands every vertex 180-degree-rotated about the mirror normal).
    #[test]
    fn rotated_instance_floor_mirror_is_true_planar_reflection() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        // Rotated, scaled, off-origin — nothing symmetric about anything.
        let src = live_instance([1.5, 4.0, -2.5], 0.8, [0.7, -1.3, 2.1]);
        let in_buf = upload(&device, &[src]);
        let out_buf = device.create_buffer_shared(2 * 32);

        let d = 1.25f32; // floor plane y = 1.25
        dispatch(&device, &pipeline, &in_buf, &out_buf, 2, 2, d, 1.0);
        let gpu_data = read_back(&out_buf, 2);

        // Expected via the true reflection, from raw linear algebra.
        let m = m_mirror(2); // +Y plane
        let r = euler_xyz([src.rot_pad[0], src.rot_pad[1], src.rot_pad[2]]);
        let r_prime = euler_xyz(conjugate_euler(
            [src.rot_pad[0], src.rot_pad[1], src.rot_pad[2]],
            2,
        ));
        let t_prime = [src.pos_scale[0], 2.0 * d - src.pos_scale[1], src.pos_scale[2]];
        for &(vx, vy, vz) in &[
            (0.4f32, -0.9, 1.2),
            (-1.1, 0.3, 0.7),
            (0.0, 1.0, 0.0),
            (2.0, -2.0, 2.0),
        ] {
            // Stored path: flip the y component, scale, rotate, translate.
            let stored = {
                let sv = [vx * src.pos_scale[3], -vy * src.pos_scale[3], vz * src.pos_scale[3]];
                let rv = mat_vec(&r_prime, sv);
                [rv[0] + t_prime[0], rv[1] + t_prime[1], rv[2] + t_prime[2]]
            };
            // True path: rotate, scale, translate, reflect about y = d.
            let truth = {
                let rv = mat_vec(&r, [vx * src.pos_scale[3], vy * src.pos_scale[3], vz * src.pos_scale[3]]);
                let world = [rv[0] + src.pos_scale[0], rv[1] + src.pos_scale[1], rv[2] + src.pos_scale[2]];
                let refl = mat_vec(&m, [world[0], world[1] - d, world[2]]);
                [refl[0], refl[1] + d, refl[2]]
            };
            for i in 0..3 {
                assert!(
                    (stored[i] - truth[i]).abs() < 1e-4,
                    "probe vertex ({vx},{vy},{vz}): stored {stored:?} != true {truth:?}"
                );
            }
        }

        // And the buffer itself carries the stored transform verbatim.
        let expected = cpu_reflect(&[src], 2, d, 1.0);
        assert_matches_cpu(&gpu_data, &expected, "floor mirror");
        assert_eq!(gpu_data[1].pos_scale[3], src.pos_scale[3], "scale stays positive");
        assert_eq!(gpu_data[1].rot_pad[3], 2.0, "marker = plane component + 1");
    }

    /// D3, unwired `in`: run() binds a one-element identity buffer, so the
    /// output is slot 0 = identity original, slot 1 = mirrored identity
    /// (reflection of the origin across the plane: 2 d s on the axis,
    /// negated scale, marker = plane component + 1).
    #[test]
    fn unwired_in_is_one_live_identity_instance() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ReflectArray>()
            .expect("reflect_array codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "reflect_array_test",
        );

        let identity = [InstanceTransform {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            rot_pad: [0.0; 4],
        }];
        let in_buf = upload(&device, &identity);
        let out_buf = device.create_buffer_shared(2 * 32);

        // -Y floor at d = 3: mirrored identity sits at y = -2 d = -6.
        dispatch(&device, &pipeline, &in_buf, &out_buf, 2, 3, 3.0, 1.0);
        let gpu_data = read_back(&out_buf, 2);
        assert_matches_cpu(&gpu_data, &cpu_reflect(&identity, 3, 3.0, 1.0), "unwired");
        assert_eq!(gpu_data[0].pos_scale, [0.0, 0.0, 0.0, 1.0], "original identity");
        assert_eq!(gpu_data[0].rot_pad, [0.0; 4], "original identity, marker 0");
        assert_eq!(gpu_data[1].pos_scale, [0.0, -6.0, 0.0, 1.0], "mirrored identity");
        assert_eq!(gpu_data[1].rot_pad, [0.0, 0.0, 0.0, 2.0], "plane comp + 1 marker");
    }
}
