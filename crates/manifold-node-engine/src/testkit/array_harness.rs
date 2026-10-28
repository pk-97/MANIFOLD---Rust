use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use crate::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::exec::backend::Backend;
use crate::exec::effect_node::{EffectNode, EffectNodeContext, FrameTime, ParamValues, SimStep};
use crate::exec::execution_plan::ResourceId;
use crate::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::parameters::ParamValue;
use crate::ports::{ArrayType, KnownItem};
use crate::{exec::metal_backend::MetalBackend, ports::PortType, ports::ScalarType};

pub struct Harness {
    pub device: manifold_gpu::testkit::TestDevice,
    pub backend: MetalBackend,
    next: u32,
    /// Each array's record layout, as a producer port would declare it.
    layouts: Vec<(Slot, ArrayType)>,
    /// Live extents the last `run` published.
    pub live_extents: Vec<(Slot, crate::scene::live_extent::LiveExtent)>,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Self {
        let device = manifold_gpu::testkit::test_device();
        let backend = MetalBackend::new(device.arc(), 1, 1, GpuTextureFormat::Rgba8Unorm);
        Self {
            device,
            backend,
            next: 0,
            layouts: Vec::new(),
            live_extents: Vec::new(),
        }
    }

    pub fn array<T: KnownItem>(&mut self, values: &[T], capacity: usize) -> (Slot, GpuBuffer) {
        let bytes = (capacity.max(values.len()).max(1) * std::mem::size_of::<T>()) as u64;
        let buffer = self.device.create_buffer(bytes, BufferUse::Upload);
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: shared buffer sized for `values`; no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        let slot = self
            .backend
            .pre_bind_array(ResourceId(self.next), buffer.clone());
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
        let slot = self
            .backend
            .acquire(ResourceId(self.next), PortType::Transform, None, (0, 0));
        self.next += 1;
        Backend::set_transform(&mut self.backend, slot, value);
        slot
    }

    /// A wired CPU value input (a vector field, say) holding `value`; replace
    /// it between runs through `backend.cpu_values_mut()`.
    pub fn cpu_input<T: Clone + Send + 'static>(&mut self, ty: PortType, value: T) -> Slot {
        let slot = self
            .backend
            .acquire(ResourceId(self.next), ty, None, (0, 0));
        self.next += 1;
        Backend::cpu_values_mut(&mut self.backend).set(slot, value);
        slot
    }

    /// One frame of `node.evaluate()` at transport 0 under the default step,
    /// committed and waited. Returns the scalar writes and the node's errors.
    pub fn run<N: EffectNode + ?Sized>(
        &mut self,
        prim: &mut N,
        inputs: &[(&'static str, Slot)],
        outputs: &[(&'static str, Slot)],
        params: &ParamValues,
    ) -> (Vec<(Slot, ParamValue)>, Vec<String>) {
        self.run_at(prim, inputs, outputs, params, 0.0, SimStep::default())
    }

    /// One frame of `node.evaluate()` at transport `seconds` under `step`.
    pub fn run_at<N: EffectNode + ?Sized>(
        &mut self,
        prim: &mut N,
        inputs: &[(&'static str, Slot)],
        outputs: &[(&'static str, Slot)],
        params: &ParamValues,
        seconds: f64,
        step: SimStep,
    ) -> (Vec<(Slot, ParamValue)>, Vec<String>) {
        let generations = vec![0_u64; self.next as usize + 1];
        let layouts: Vec<(&'static str, ArrayType)> = inputs
            .iter()
            .filter_map(|&(port, slot)| {
                self.layouts
                    .iter()
                    .find(|(s, _)| *s == slot)
                    .map(|&(_, layout)| (port, layout))
            })
            .collect();
        let mut scalars = Vec::new();
        let mut errors = Vec::new();
        self.live_extents.clear();
        {
            let (mut camera, mut light, mut material, mut transform) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let (mut atmosphere, mut render_mode, mut object) =
                (Vec::new(), Vec::new(), Vec::new());
            let backend: &dyn Backend = &self.backend;
            let node_inputs =
                NodeInputs::new(inputs, backend, &generations).with_array_layouts(&layouts);
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
                    seconds: Seconds(seconds),
                    delta: Seconds(1.0 / 60.0),
                    frame_count: 0,
                };
                let mut ctx =
                    EffectNodeContext::new(time, params, node_inputs, node_outputs, Some(&mut gpu))
                        .with_sim_step(step)
                        .with_errors(&mut errors);
                prim.evaluate(&mut ctx);
            }
            native.commit_and_wait().expect("GPU work completes");
        }
        // Storage a node provides replaces its slot's, as the executor installs it.
        for &(port, slot) in outputs {
            if prim.provides_array_output(port)
                && let Some(buffer) = prim.provided_array_output(port)
            {
                assert!(
                    Backend::install_array_buffer(&mut self.backend, slot, buffer.clone()),
                    "{port}: install"
                );
            }
        }
        (scalars, errors)
    }

    /// The storage a slot holds now: a provided output's, after its run.
    pub fn buffer(&self, slot: Slot) -> GpuBuffer {
        self.backend.array_buffer(slot).expect("array slot").clone()
    }
}

pub fn read<T: bytemuck::Pod>(buffer: &GpuBuffer, count: usize) -> Vec<T> {
    let bytes = manifold_gpu::testkit::read_bytes(buffer, count * std::mem::size_of::<T>());
    bytes
        .chunks_exact(std::mem::size_of::<T>())
        .map(bytemuck::pod_read_unaligned)
        .collect()
}

pub fn params(values: &[(&'static str, f32)]) -> ParamValues {
    let mut params = ParamValues::default();
    for &(name, value) in values {
        params.insert(Cow::Borrowed(name), ParamValue::Float(value));
    }
    params
}
