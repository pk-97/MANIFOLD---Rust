use std::borrow::Cow;

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use crate::water::primitives::sort_particles_into_cells::SortParticlesIntoCells;
use crate::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::exec::backend::Backend;
use crate::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use crate::exec::execution_plan::ResourceId;
use crate::water::fluid_particles::{CellRange, FluidBlob, FluidParticle, bin_counts};
use crate::parameters::ParamValue;
use crate::ports::{ArrayType, KnownItem};
use crate::primitive::Primitive;
use crate::{exec::metal_backend::MetalBackend, ports::PortType, ports::ScalarType};

pub(crate) struct Harness {
    pub device: manifold_gpu::testkit::TestDevice,
    pub backend: MetalBackend,
    next: u32,
    /// Each array's record layout, as a producer port would declare it.
    layouts: Vec<(Slot, ArrayType)>,
    /// Live extents the last `run` published.
    pub live_extents: Vec<(Slot, crate::scene::live_extent::LiveExtent)>,
}

impl Harness {
    pub fn new() -> Self {
        let device = manifold_gpu::testkit::test_device();
        let backend = MetalBackend::new(device.arc(), 1, 1, GpuTextureFormat::Rgba8Unorm);
        Self { device, backend, next: 0, layouts: Vec::new(), live_extents: Vec::new() }
    }

    pub fn array<T: KnownItem>(&mut self, values: &[T], capacity: usize) -> (Slot, GpuBuffer) {
        let bytes = (capacity.max(values.len()).max(1) * std::mem::size_of::<T>()) as u64;
        let buffer = self.device.create_buffer_shared(bytes);
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: shared buffer sized for `values`; no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        let slot = self.backend.pre_bind_array(ResourceId(self.next), buffer.clone());
        self.next += 1;
        self.layouts.push((slot, ArrayType::of_known::<T>()));
        (slot, buffer)
    }

    pub fn scalar(&mut self) -> Slot {
        let slot = self.backend.acquire(
            ResourceId(self.next),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        self.next += 1;
        slot
    }

    /// A wired scalar input holding `value`.
    pub fn scalar_input(&mut self, value: f32) -> Slot {
        let slot = self.scalar();
        self.backend.set_scalar(slot, ParamValue::Float(value));
        slot
    }

    /// A wired transform input holding `value`.
    pub fn transform_input(&mut self, value: crate::scene::transform::Transform) -> Slot {
        let slot = self.backend.acquire(ResourceId(self.next), PortType::Transform, None, (0, 0));
        self.next += 1;
        Backend::set_transform(&mut self.backend, slot, value);
        slot
    }

    /// One frame of `prim.run()`, committed and waited. Returns the scalar
    /// writes and the node's errors.
    pub fn run<P: Primitive>(
        &mut self,
        prim: &mut P,
        inputs: &[(&'static str, Slot)],
        outputs: &[(&'static str, Slot)],
        params: &ParamValues,
    ) -> (Vec<(Slot, ParamValue)>, Vec<String>) {
        let generations = vec![0_u64; self.next as usize + 1];
        let layouts: Vec<(&'static str, ArrayType)> = inputs
            .iter()
            .filter_map(|&(port, slot)| {
                self.layouts.iter().find(|(s, _)| *s == slot).map(|&(_, layout)| (port, layout))
            })
            .collect();
        let mut scalars = Vec::new();
        let mut errors = Vec::new();
        self.live_extents.clear();
        {
            let (mut camera, mut light, mut material, mut transform) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let (mut atmosphere, mut render_mode, mut object) = (Vec::new(), Vec::new(), Vec::new());
            let backend: &dyn Backend = &self.backend;
            let node_inputs = NodeInputs::new(inputs, backend, &generations).with_array_layouts(&layouts);
            let node_outputs = NodeOutputs::new(
                outputs,
                backend,
                &mut scalars,
                &mut camera,
                &mut light,
                &mut material,
                &mut transform,
                &mut atmosphere,
                &mut render_mode,
                &mut object,
            )
            .with_live_extent_writes(&mut self.live_extents);
            let mut native = self.device.create_encoder("liquid surface atom test");
            {
                let mut gpu = RendererGpuEncoder::new(&mut native, &self.device);
                let time = FrameTime {
                    beats: Beats(0.0),
                    seconds: Seconds(0.0),
                    delta: Seconds(1.0 / 60.0),
                    frame_count: 0,
                };
                let mut ctx = EffectNodeContext::new(time, params, node_inputs, node_outputs, Some(&mut gpu))
                    .with_errors(&mut errors);
                Primitive::run(prim, &mut ctx);
            }
            native.commit_and_wait_completed();
        }
        // Storage a node provides replaces its slot's, as the executor installs it.
        for &(port, slot) in outputs {
            if prim.provides_array_output(port)
                && let Some(buffer) = prim.provided_array_output(port)
            {
                assert!(Backend::install_array_buffer(&mut self.backend, slot, buffer.clone()), "{port}: install");
            }
        }
        (scalars, errors)
    }

    /// The storage a slot holds now: a provided output's, after its run.
    pub fn buffer(&self, slot: Slot) -> GpuBuffer {
        self.backend.array_buffer(slot).expect("array slot").clone()
    }
}

pub(crate) fn read<T: bytemuck::Pod>(buffer: &GpuBuffer, count: usize) -> Vec<T> {
    let ptr = buffer.mapped_ptr().expect("shared buffer");
    // SAFETY: shared buffer holding at least `count` elements; GPU work done.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, count * std::mem::size_of::<T>()) };
    bytemuck::cast_slice(bytes).to_vec()
}

pub(crate) fn params(values: &[(&'static str, f32)]) -> ParamValues {
    let mut params = ParamValues::default();
    for &(name, value) in values {
        params.insert(Cow::Borrowed(name), ParamValue::Float(value));
    }
    params
}

/// Deterministic pseudo-random stream (xorshift) for fixtures.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self { Self(seed) }
    pub fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

pub(crate) struct Lattice {
    pub center: [f32; 3],
    pub size: [f32; 3],
    pub cell: f32,
}

impl Lattice {
    /// The box, and the bin grid the sort publishes for it (searchers take
    /// it as params here, as they take the sort's wires in a graph).
    pub fn params(&self, extra: &[(&'static str, f32)]) -> ParamValues {
        let bins = bin_counts(self.size, self.cell);
        let mut values = vec![
            ("center_x", self.center[0]),
            ("center_y", self.center[1]),
            ("center_z", self.center[2]),
            ("size_x", self.size[0]),
            ("size_y", self.size[1]),
            ("size_z", self.size[2]),
            ("cell_size", self.cell),
            ("bins_x", bins[0] as f32),
            ("bins_y", bins[1] as f32),
            ("bins_z", bins[2] as f32),
        ];
        values.extend_from_slice(extra);
        params(&values)
    }

    pub fn min(&self) -> [f32; 3] {
        std::array::from_fn(|axis| self.center[axis] - 0.5 * self.size[axis])
    }

    /// The sort's bin rule, evaluated with the kernel's f32 operations.
    pub fn bin(&self, p: [f32; 3]) -> usize {
        let bins = bin_counts(self.size, self.cell);
        let min = self.min();
        let inv = 1.0 / self.cell;
        let b: [usize; 3] = std::array::from_fn(|axis| {
            (((p[axis] - min[axis]) * inv).floor() as i64).clamp(0, i64::from(bins[axis]) - 1) as usize
        });
        b[0] + bins[0] as usize * (b[1] + bins[1] as usize * b[2])
    }
}

pub(crate) fn particle(position: [f32; 3], radius: f32, id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], radius],
        velocity: [0.0; 3],
        id,
    }
}

/// Sort then blobs, read back: (sorted, ranges, blobs, their GPU slots in that order).
type Shaped = (Vec<FluidParticle>, Vec<CellRange>, Vec<FluidBlob>, (Slot, Slot, Slot));

pub(crate) fn sort_and_shape(
    harness: &mut Harness,
    lattice: &Lattice,
    particles: &[FluidParticle],
    count: usize,
    shape: &[(&'static str, f32)],
) -> Shaped {
    let bins = bin_counts(lattice.size, lattice.cell);
    let bin_total = bins.iter().product::<u32>() as usize;
    let (input, _) = harness.array(particles, particles.len());
    let (sorted_slot, sorted_buf) = harness.array::<FluidParticle>(&[], particles.len());
    let (ranges_slot, _) = harness.array::<CellRange>(&[], 1);
    let count_slot = harness.scalar_input(count as f32);
    let mut sort = SortParticlesIntoCells::new();
    let (_, errors) = harness.run(
        &mut sort,
        &[("particles", input), ("count", count_slot)],
        &[("sorted", sorted_slot), ("cell_ranges", ranges_slot)],
        &lattice.params(&[]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let (blobs_slot, blobs_buf) = harness.array::<FluidBlob>(&[], particles.len());
    let mut shape_node = crate::water::primitives::testkit::shape_particle_blobs();
    let (_, errors) = harness.run(
        &mut shape_node,
        &[("sorted", sorted_slot), ("cell_ranges", ranges_slot)],
        &[("blobs", blobs_slot)],
        &lattice.params(shape),
    );
    assert!(errors.is_empty(), "{errors:?}");
    (
        read(&sorted_buf, particles.len()),
        read(&harness.buffer(ranges_slot), bin_total),
        read(&blobs_buf, particles.len()),
        (sorted_slot, ranges_slot, blobs_slot),
    )
}


pub(crate) fn blob_matrix(blob: &FluidBlob) -> [[f64; 3]; 3] {
    let d = blob.shape_diag.map(f64::from);
    let o = blob.shape_off.map(f64::from);
    [[d[0], o[0], o[1]], [o[0], d[1], o[2]], [o[1], o[2], d[2]]]
}

/// The clamp's wires as params: the box, the level-set lattice, the solid
/// lattice and the bin size.
pub(crate) fn clamp_params(center: [f32; 3], size: [f32; 3], nodes: [u32; 3], solid_nodes: [u32; 3], cell: f32) -> ParamValues {
    params(&[
        ("center_x", center[0]),
        ("center_y", center[1]),
        ("center_z", center[2]),
        ("size_x", size[0]),
        ("size_y", size[1]),
        ("size_z", size[2]),
        ("nodes_x", nodes[0] as f32),
        ("nodes_y", nodes[1] as f32),
        ("nodes_z", nodes[2] as f32),
        ("solid_nodes_x", solid_nodes[0] as f32),
        ("solid_nodes_y", solid_nodes[1] as f32),
        ("solid_nodes_z", solid_nodes[2] as f32),
        ("cell_size", cell),
    ])
}

/// The level set's cap outside the liquid, as a fraction of a bin; the WGSL of
/// `node.particle_volume` and `node.shape_particle_blobs` both hold it (P6e).
/// The volume's cap, as a fraction of a bin; the blob reach cap is the rest.
pub(crate) fn native_support(ijk: [u32; 3], centre: [f64; 3], radius: f64, min: [f64; 3], h: [f64; 3], extra: f64) -> bool {
    (0..3).all(|a| {
        let lo = ((centre[a] - 1.5 * radius - extra - min[a]) / h[a]).floor();
        let hi = ((centre[a] + 1.5 * radius + extra - min[a]) / h[a]).floor() + 1.0;
        f64::from(ijk[a]) >= lo && f64::from(ijk[a]) <= hi
    })
}

/// f64 trilinear sample of a solid lattice spanning `min`..`min + size`: the
/// rule node.particle_volume and node.clamp_liquid_to_solids share.
pub(crate) fn solid_sample(solid: &[f32], nodes: [u32; 3], min: [f32; 3], size: [f32; 3], p: [f64; 3]) -> f64 {
    let n = nodes.map(|v| v as usize);
    let mut base = [0usize; 3];
    let mut frac = [0f64; 3];
    for a in 0..3 {
        let spacing = f64::from(size[a]) / (n[a] - 1) as f64;
        let g = ((p[a] - f64::from(min[a])) / spacing).clamp(0.0, (n[a] - 1) as f64);
        base[a] = (g.floor() as usize).min(n[a] - 2);
        frac[a] = g - base[a] as f64;
    }
    (0..8usize)
        .map(|corner| {
            let o = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let w: f64 = (0..3).map(|a| if o[a] == 1 { frac[a] } else { 1.0 - frac[a] }).product();
            let at: [usize; 3] = std::array::from_fn(|a| base[a] + o[a]);
            w * f64::from(solid[at[0] + n[0] * (at[1] + n[1] * at[2])])
        })
        .sum()
}
