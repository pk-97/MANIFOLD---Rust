//! `node.move_swarm` — advance each swarm particle through the frame's fixed
//! ticks under the scene's force lattice: v += a·dt, x += v·dt per tick.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::Particle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{active_elements, standalone_pipeline};

use crate::force_lattice::FORCE_LATTICE;

/// Generated uniform layout: the params in order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MoveSwarmUniforms {
    pub(crate) active_count: i32,
    pub(crate) ticks: i32,
    pub(crate) first_tick: i32,
    pub(crate) force_lattices: i32,
    pub(crate) field_nodes_x: i32,
    pub(crate) field_nodes_y: i32,
    pub(crate) field_nodes_z: i32,
    pub(crate) field_spacing: f32,
    pub(crate) field_min_x: f32,
    pub(crate) field_min_y: f32,
    pub(crate) field_min_z: f32,
    pub(crate) tick_seconds: f32,
    pub(crate) dispatch_count: u32,
    pub(crate) _pad: [u32; 3],
}

const fn int(name: &'static str, label: &'static str, default: f32, max: f32) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty: ParamType::Int, default: ParamValue::Float(default), range: Some((0.0, max)), enum_values: &[] }
}

const fn float(name: &'static str, label: &'static str, default: f32, range: (f32, f32)) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty: ParamType::Float, default: ParamValue::Float(default), range: Some(range), enum_values: &[] }
}

manifold_node_engine::primitive! {
    name: MoveSwarm,
    type_id: "node.move_swarm",
    purpose: "Advance each live particle through this frame's fixed ticks under the scene's force lattice: per tick, a = the trilinear read of that tick's lattice at the particle, then v += a·dt and x += v·dt (semi-implicit Euler, dt = tick_seconds). Each tick reads its own lattice (one per tick from first_tick, or one for every tick when force_lattices is 1; 0 or forces unwired: no force), so the swarm moves the same at any frame rate. Dead particles (life <= 0) pass through unchanged.",
    inputs: {
        in: Array(Particle) required,
        forces: Array(f32) optional,
        active_count: ScalarF32 optional,
        ticks: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional,
        field_nodes_y: ScalarF32 optional,
        field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        field_min_x: ScalarF32 optional,
        field_min_y: ScalarF32 optional,
        field_min_z: ScalarF32 optional,
        tick_seconds: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        int("active_count", "Active Count", 100_000.0, 16_000_000.0),
        int("ticks", "Ticks", 0.0, 4096.0),
        int("first_tick", "First Tick", 0.0, 16_777_216.0),
        int("force_lattices", "Force Lattices", 0.0, 4096.0),
        int("field_nodes_x", "Field Nodes X", 2.0, 4096.0),
        int("field_nodes_y", "Field Nodes Y", 2.0, 4096.0),
        int("field_nodes_z", "Field Nodes Z", 2.0, 4096.0),
        float("field_spacing", "Field Spacing", 0.25, (1.0e-4, 400.0)),
        float("field_min_x", "Field Min X", 0.0, (-1.0e4, 1.0e4)),
        float("field_min_y", "Field Min Y", 0.0, (-1.0e4, 1.0e4)),
        float("field_min_z", "Field Min Z", 0.0, (-1.0e4, 1.0e4)),
        float("tick_seconds", "Tick Seconds", 1.0 / 60.0, (0.0, 1.0)),
    ],
    depth_rule: Terminal,
    composition_notes: "The swarm integrator. Wire forces and every scalar but active_count from node.swarm_forces (its ticks, first_tick, force_lattices, field_nodes_x/y/z, field_spacing, field_min_x/y/z and tick_seconds), particles from a seeder held across frames, then node.swarm_to_copies into render_copies. in/out alias one buffer. Scene forces reach it through node.swarm_forces's acceleration_field, the same force list water and rigid bodies read.",
    examples: ["Swarm"],
    picker: { label: "Move Swarm", category: Atom },
    summary: "Moves every swarm particle by the scene's forces, one fixed step per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["swarm", "move swarm", "particle forces", "force particles"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/move_swarm_body.wgsl"),
    input_access: [Coincident, BufferGather],
    wgsl_includes: [FORCE_LATTICE],
}

impl Primitive for MoveSwarm {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out").then(|| input_capacities.iter().find(|(p, _)| *p == "in").map(|&(_, n)| n)).flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as i32;
        let requested = int(ctx, "active_count", 100_000.0) as u32;
        let ticks = int(ctx, "ticks", 0.0);
        let first_tick = int(ctx, "first_tick", 0.0);
        let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"].map(|name| int(ctx, name, 2.0).max(2));
        let spacing = ctx.scalar_or_param("field_spacing", 0.25);
        let min = ["field_min_x", "field_min_y", "field_min_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let tick_seconds = ctx.scalar_or_param("tick_seconds", 1.0 / 60.0);
        let Some(particles) = ctx.inputs.array("in") else { return };
        let forces = ctx.inputs.array("forces");
        let lattice_bytes = nodes.iter().map(|&n| n as u64).product::<u64>() * 16;
        let force_lattices = forces.map_or(0, |forces| int(ctx, "force_lattices", 0.0).min((forces.size / lattice_bytes) as i32));
        if force_lattices > 0 && !(spacing.is_finite() && spacing > 0.0) {
            ctx.error("Move Swarm: field_spacing must be positive".to_string());
            return;
        }
        let active = active_elements::<Particle>(particles.size, requested);
        if active == 0 || ticks == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = MoveSwarmUniforms {
            active_count: active as i32,
            ticks,
            first_tick,
            force_lattices,
            field_nodes_x: nodes[0],
            field_nodes_y: nodes[1],
            field_nodes_z: nodes[2],
            field_spacing: spacing,
            field_min_x: min[0],
            field_min_y: min[1],
            field_min_z: min[2],
            tick_seconds,
            dispatch_count: active,
            _pad: [0; 3],
        };
        // `in`/`out` alias the particle buffer; an unwired lattice is never
        // read (force_lattices 0), so the particles stand in for it.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: forces.unwrap_or(particles), offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: particles, offset: 0 },
            ],
            [active.div_ceil(256), 1, 1],
            "node.move_swarm",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_swarm_generates_a_valid_gathering_kernel() {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<MoveSwarm>().expect("move_swarm codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("buf_forces: array<f32>"), "{wgsl}");
        assert!(wgsl.contains("tick_seconds: f32,\n    dispatch_count: u32,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<MoveSwarmUniforms>(), 64);
    }
}

/// One GPU proof (SCENE_SETUP_PROGRAMME.md E1): a vortex laid on the shared
/// force lattice spins 100k swarm particles, and every particle lands where
/// the lattice's CPU twin integrates it.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::testkit::array_harness::{params, read, Harness};
    use manifold_physics::force_lattice::ForceLattice;
    use manifold_physics::{FieldValue, VectorField};

    const COUNT: usize = 100_000;
    const FRAMES: u32 = 60;
    const DT: f32 = 1.0 / 60.0;

    fn particle(i: usize) -> Particle {
        // A golden-angle disc of radii 0.3..1.8 m, heights −1..1 m, at rest.
        let t = i as f32 / COUNT as f32;
        let angle = i as f32 * 2.399_963;
        let radius = 0.3 + 1.5 * t.sqrt();
        Particle {
            position: [radius * angle.cos(), 2.0 * ((i * 7919) % COUNT) as f32 / COUNT as f32 - 1.0, radius * angle.sin()],
            life: 1.0,
            ..bytemuck::Zeroable::zeroed()
        }
    }

    #[test]
    fn vortex_lattice_spins_100k_swarm_particles_as_the_field_says() {
        let lattice = ForceLattice::over_box([-2.0; 3], [4.0; 3], 0.25);
        let field = FieldValue::vortex([0.0; 3], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap();
        let mut values = vec![[0.0f32; 4]; lattice.node_count()];
        lattice.fill(&mut values, |x| field.sample(x)).unwrap();
        let start: Vec<Particle> = (0..COUNT).map(particle).collect();

        let mut h = Harness::new();
        let (particles, buffer) = h.array(&start, COUNT);
        let flat: Vec<f32> = values.iter().flatten().copied().collect();
        let (forces, _) = h.array(&flat, flat.len());
        let [nx, ny, nz] = lattice.nodes().map(|n| n as f32);
        let mut node = MoveSwarm::new();
        let mut costs = Vec::with_capacity(FRAMES as usize);
        for frame in 0..FRAMES {
            let p = params(&[
                ("active_count", COUNT as f32),
                ("ticks", 1.0),
                ("first_tick", frame as f32),
                ("force_lattices", 1.0),
                ("field_nodes_x", nx),
                ("field_nodes_y", ny),
                ("field_nodes_z", nz),
                ("field_spacing", lattice.spacing()),
                ("field_min_x", -2.0),
                ("field_min_y", -2.0),
                ("field_min_z", -2.0),
                ("tick_seconds", DT),
            ]);
            let began = std::time::Instant::now();
            let (_, errors) = h.run(&mut node, &[("in", particles), ("forces", forces)], &[("out", particles)], &p);
            costs.push(began.elapsed().as_secs_f64() * 1000.0);
            assert!(errors.is_empty(), "{errors:?}");
        }
        costs.sort_by(f64::total_cmp);
        println!("move_swarm 100k particles, one tick: median frame {:.3} ms (encode + GPU + wait), worst {:.3} ms", costs[costs.len() / 2], costs[costs.len() - 1]);

        let moved: Vec<Particle> = read(&buffer, COUNT);
        let mut spin = 0.0f64;
        for (i, (got, from)) in moved.iter().zip(&start).enumerate() {
            let (mut x, mut v) = (from.position, from.velocity);
            for _ in 0..FRAMES {
                let a = lattice.sample(&values, x);
                for d in 0..3 {
                    v[d] += a[d] * DT;
                    x[d] += v[d] * DT;
                }
            }
            for d in 0..3 {
                assert!((got.position[d] - x[d]).abs() <= 1e-3 * (1.0 + x[d].abs()), "particle {i} axis {d}: {} vs {}", got.position[d], x[d]);
                assert!((got.velocity[d] - v[d]).abs() <= 1e-3 * (1.0 + v[d].abs()), "particle {i} velocity {d}: {} vs {}", got.velocity[d], v[d]);
            }
            // Angular momentum about the vortex axis (y): z·vx − x·vz.
            spin += f64::from(got.position[2] * got.velocity[0] - got.position[0] * got.velocity[2]);
        }
        let expected = {
            let a = field.sample([1.0, 0.0, 0.0]);
            f64::from(a[2]).signum()
        };
        assert!(expected != 0.0 && (-spin).signum() == expected, "the swarm turns the way the vortex pushes: spin {spin}");
    }
}
