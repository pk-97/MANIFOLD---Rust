//! P2 test-only atom oracle. No fused uniform or phase code is used here.
use super::*;
use std::cell::Cell;
use super::super::turbulence_emission_count::TurbulenceEmissionCount;
use super::super::dust_potential::DustPotential;
use super::super::whitewater_emitter_velocity::WhitewaterEmitterVelocity;
use super::super::inside_turbulence_potential::InsideTurbulencePotential;
use super::super::jitter_particles::JitterParticles;
use super::super::sample_faces_at_particles::SampleFacesAtParticles;
use super::super::wavecrest_potential::WavecrestPotential;
use super::super::energy_potential::EnergyPotential;

#[derive(Default)]
pub(super) struct Reference {
    pub enabled: bool,
    pub capture: bool,
    pub emit_dispatches: Cell<u32>,
    pub dust_dispatches: Cell<u32>,
    slots: u32,
    scratch: Option<[GpuBuffer; 5]>,
    dust_energy: Option<GpuBuffer>,
    pub snapshots: Option<[GpuBuffer; 6]>,
    jitter: Option<GpuComputePipeline>,
    sample: Option<GpuComputePipeline>,
    emitter_velocity: Option<GpuComputePipeline>,
    energy: Option<GpuComputePipeline>,
    wavecrest: Option<GpuComputePipeline>,
    inside: Option<GpuComputePipeline>,
    emission: Option<GpuComputePipeline>,
    dust: Option<GpuComputePipeline>,
}

impl Reference {
    pub fn record_dispatch(&self, label: &str, count: u32) {
        if count == 0 { return; }
        let counter = if label.starts_with("node.whitewater_step.dust") { &self.dust_dispatches } else { &self.emit_dispatches };
        counter.set(counter.get() + 1);
    }

    fn atom<P: Primitive>(&self, enc: &mut manifold_gpu::GpuEncoder, pipeline: &GpuComputePipeline,
        values: &[(&str, f32)], buffers: &[&GpuBuffer], count: u32, label: &str) {
        self.atom_then::<P>(enc, pipeline, values, buffers, count, label, Barrier::After);
    }

    fn atom_then<P: Primitive>(&self, enc: &mut manifold_gpu::GpuEncoder, pipeline: &GpuComputePipeline,
        values: &[(&str, f32)], buffers: &[&GpuBuffer], count: u32, label: &str, barrier: Barrier) {
        atom_then::<P>(enc, pipeline, values, buffers, count, label, barrier);
        self.record_dispatch(label, count);
    }
    pub fn print_pipeline_limits(&self) {
        for pipeline in [&self.jitter, &self.sample, &self.emitter_velocity, &self.energy,
            &self.wavecrest, &self.inside, &self.emission, &self.dust] {
            let pipeline = get(pipeline);
            println!("reference {}: max_threads={:?}", pipeline.label, pipeline.max_threads_per_threadgroup());
        }
    }
    pub fn output(&self, port: &str) -> Option<&GpuBuffer> {
        let i = ["proof_sampled", "proof_unscaled", "proof_energy", "proof_counts", "proof_dust_energy", "proof_dust_counts"]
            .iter().position(|p| *p == port)?;
        assert!(self.capture);
        assert!(self.emit_dispatches.get() > 0, "emitter path never dispatched");
        if i >= 4 { assert!(self.dust_dispatches.get() > 0, "dust path never dispatched"); }
        Some(&self.snapshots.as_ref().expect("snapshots reserved")[i])
    }
    pub fn reserve(&mut self, device: &GpuDevice, slots: u32) -> Result<(), String> {
        if !self.enabled && !self.capture { return Ok(()); }
        standalone_pipeline::<JitterParticles>(&mut self.jitter, device);
        standalone_pipeline::<SampleFacesAtParticles>(&mut self.sample, device);
        standalone_pipeline::<WhitewaterEmitterVelocity>(&mut self.emitter_velocity, device);
        standalone_pipeline::<EnergyPotential>(&mut self.energy, device);
        standalone_pipeline::<WavecrestPotential>(&mut self.wavecrest, device);
        standalone_pipeline::<InsideTurbulencePotential>(&mut self.inside, device);
        standalone_pipeline::<TurbulenceEmissionCount>(&mut self.emission, device);
        standalone_pipeline::<DustPotential>(&mut self.dust, device);

        if self.scratch.is_none() || self.slots < slots {
            let n = u64::from(slots);
            let alloc = |bytes| allocate(device, bytes, false);
            self.scratch = Some([alloc(n * PARTICLE)?, alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * 4)?, alloc(n * 4)?]);
            self.dust_energy = Some(alloc(n * 4)?);
            self.snapshots = Some([alloc(n * PARTICLE)?, alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * 4)?, alloc(n * 4)?, alloc(n * 4)?]);
            self.slots = slots;
        }
        Ok(())
    }
    pub fn scratch(&self) -> &[GpuBuffer; 5] { self.scratch.as_ref().expect("oracle reserved") }
    pub fn dust_energy(&self) -> &GpuBuffer { self.dust_energy.as_ref().expect("oracle reserved") }
    pub fn capture_emit(&self, enc: &mut manifold_gpu::GpuEncoder, sampled: &GpuBuffer, unscaled: &GpuBuffer,
        energy: &GpuBuffer, counts: &GpuBuffer, emitters: u32, dust: bool) {
        if !self.capture { return; }
        let dst = self.snapshots.as_ref().expect("snapshots reserved");
        for (i, src) in [sampled, unscaled, energy, counts].into_iter().enumerate() {
            enc.clear_buffer(&dst[i]);
            if emitters > 0 && (i != 1 || dust) {
                enc.copy_buffer_to_buffer(src, &dst[i], u64::from(emitters) * if i < 2 { PARTICLE } else { 4 });
            }
        }
    }
    pub fn capture_dust(&self, enc: &mut manifold_gpu::GpuEncoder, energy: &GpuBuffer, counts: &GpuBuffer, emitters: u32) {
        if !self.capture { return; }
        let dst = self.snapshots.as_ref().expect("snapshots reserved");
        enc.clear_buffer(&dst[4]);
        enc.clear_buffer(&dst[5]);
        if emitters > 0 {
            enc.copy_buffer_to_buffer(energy, &dst[4], u64::from(emitters) * 4);
            enc.copy_buffer_to_buffer(counts, &dst[5], u64::from(emitters) * 4);
        }
    }
    pub fn emit_reference(&self, enc: &mut manifold_gpu::GpuEncoder, frame: &StepFrame, inputs: &StepInputs<'_>,
        f: &Fields, surface: &GpuBuffer, curvature: usize, influence_next: usize, offsets: &GpuBuffer, emitters: u32) {
        let s = &frame.shape;
        let p = self;
        let [jittered, sampled, energy, wavecrest, inside] = self.scratch();
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [cx, cy, cz] = s.center;
        let [sx, sy, sz] = s.size;
        let [fx, fy, fz] = s.face_cells.map(|n| n as f32);
        let nodes = [("nodes_x", nx), ("nodes_y", ny), ("nodes_z", nz)];
        let box3 = [("center_x", cx), ("center_y", cy), ("center_z", cz), ("size_x", sx), ("size_y", sy), ("size_z", sz)];
        let faces = [("face_cells_x", fx), ("face_cells_y", fy), ("face_cells_z", fz)];
        let epoch = frame.epoch as f32;
        // The per-particle passes run over the emitters, not every slot of
        // the particle array: the emission count masks everything from the
        // live count up, and the spawn reads the scratch only below the
        // emission scan's length.
        self.atom::<JitterParticles>(
            enc,
            get(&p.jitter),
            &[("cell_size", s.cell_size), ("seed", frame.seed), ("epoch", epoch)],
            &[inputs.particles, jittered],
            emitters,
            "node.whitewater_step.jitter",
        );
        let mut sample = [("", 0.0); 12];
        sample[..3].copy_from_slice(&faces);
        sample[3..9].copy_from_slice(&box3);
        sample[9..].copy_from_slice(&nodes);
        self.atom::<SampleFacesAtParticles>(
            enc,
            get(&p.sample),
            &sample,
            &[jittered, inputs.faces[0], inputs.faces[1], inputs.faces[2], sampled],
            emitters,
            "node.whitewater_step.sample_velocity",
        );
        let mut grid = [("", 0.0); 9];
        grid[..6].copy_from_slice(&box3);
        grid[6..].copy_from_slice(&nodes);
        let mut velocity_params = [("", 0.0); 12];
        velocity_params[..9].copy_from_slice(&grid);
        velocity_params[9] = ("spray_speed", frame.spray_speed);
        velocity_params[10] = ("seed", frame.seed);
        velocity_params[11] = ("epoch", epoch);
        self.atom::<WhitewaterEmitterVelocity>(enc, get(&p.emitter_velocity), &velocity_params,
            &[sampled, surface, &f.cells, jittered], emitters, "node.whitewater_step.emitter_velocity");
        let sampled = jittered;
        self.atom_then::<EnergyPotential>(
            enc, get(&p.energy),
            &[("min_energy", frame.min_energy), ("max_energy", frame.max_energy)],
            &[sampled, energy], emitters, "node.whitewater_step.energy", Barrier::None);
        self.atom::<WavecrestPotential>(
            enc,
            get(&p.wavecrest),
            &grid,
            &[sampled, surface, &f.curvature[curvature], &f.cells, wavecrest],
            emitters,
            "node.whitewater_step.wavecrest",
        );
        let mut turbulence_params = [("", 0.0); 12];
        turbulence_params[..9].copy_from_slice(&grid);
        turbulence_params[9] = ("min_turbulence", frame.min_turbulence);
        turbulence_params[10] = ("max_turbulence", frame.max_turbulence);
        turbulence_params[11] = ("inside_enabled", f32::from(u8::from(frame.inside_emission)));
        self.atom::<InsideTurbulencePotential>(enc, get(&p.inside), &turbulence_params,
            &[sampled, surface, &f.turbulence, &f.cells, inside], emitters, "node.whitewater_step.inside");
        let mut count_params = [("", 0.0); 18];
        count_params[..8].copy_from_slice(&[("rate", frame.wavecrest_emission), ("turbulence_rate", frame.turbulence_emission), ("generation_rate", frame.generation_rate), ("seed", frame.seed), ("epoch", epoch), ("points_per_cell", 8.0), ("ticks", frame.ticks as f32), ("live_count", emitters as f32)]);
        count_params[8..17].copy_from_slice(&grid);
        count_params[17] = ("dt", frame.dt);
        self.atom::<TurbulenceEmissionCount>(
            enc,
            get(&p.emission),
            &count_params,
            &[sampled, energy, wavecrest, inside, &f.influence[influence_next], offsets],
            emitters,
            "node.whitewater_step.emission",
        );
    }
    pub fn dust_reference(&self, enc: &mut manifold_gpu::GpuEncoder, frame: &StepFrame, inputs: &StepInputs<'_>,
        f: &Fields, influence_next: usize, offsets: &GpuBuffer, emitters: u32) {
        let s = &frame.shape;
        let p = self;
        let [_, unscaled, _, wavecrest, inside] = self.scratch();
        let energy = self.dust_energy();
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [cx, cy, cz] = s.center;
        let [sx, sy, sz] = s.size;
        let nodes = [("nodes_x", nx), ("nodes_y", ny), ("nodes_z", nz)];
        let box3 = [("center_x", cx), ("center_y", cy), ("center_z", cz), ("size_x", sx), ("size_y", sy), ("size_z", sz)];
        let epoch = frame.epoch as f32;
        let mut grid = [("", 0.0); 9];
        grid[..6].copy_from_slice(&box3);
        grid[6..].copy_from_slice(&nodes);
        let mut count_params = [("", 0.0); 18];
        count_params[..8].copy_from_slice(&[("rate", frame.wavecrest_emission), ("turbulence_rate", frame.turbulence_emission), ("generation_rate", frame.generation_rate), ("seed", frame.seed), ("epoch", epoch), ("points_per_cell", 8.0), ("ticks", frame.ticks as f32), ("live_count", emitters as f32)]);
        count_params[8..17].copy_from_slice(&grid);
        count_params[17] = ("dt", frame.dt);
        let mut dust_params = [("", 0.0); 13];
        dust_params[..9].copy_from_slice(&grid);
        dust_params[9..].copy_from_slice(&[("min_turbulence", frame.min_turbulence), ("max_turbulence", frame.max_turbulence),
            ("dust_enabled", 1.0), ("boundary_dust", f32::from(u8::from(frame.boundary_dust)))]);
        self.atom::<DustPotential>(enc, get(&p.dust), &dust_params,
            &[unscaled, inputs.solid, &f.turbulence, inputs.obstacle_source.expect("validated dust source"), inside],
            emitters, "node.whitewater_step.dust_potential");
        self.atom::<EnergyPotential>(enc, get(&p.energy),
            &[("min_energy", frame.min_energy), ("max_energy", frame.max_energy)],
            &[unscaled, energy], emitters, "node.whitewater_step.dust_energy");
        count_params[0] = ("rate", 0.0);
        count_params[1] = ("turbulence_rate", frame.dust_rate);
        count_params[3] = ("seed", frame.seed + 104729.0);
        self.atom::<TurbulenceEmissionCount>(enc, get(&p.emission), &count_params,
            &[unscaled, energy, wavecrest, inside, &f.influence[influence_next], offsets], emitters, "node.whitewater_step.dust_count");
    }
}
