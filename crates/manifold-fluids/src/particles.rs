//! Particle frames written straight into caller memory: the producer side of
//! the GPU surface seam (GPU_FLUID_SURFACE_DESIGN.md section 3 (The
//! particle-frame contract)). The caller owns the destination, typically a
//! mapped GPU buffer; capture never advances the solver and does not allocate
//! once this world's scratch is warm.

use std::ffi::c_void;

use super::{FluidError, FluidWorld, native_result};

/// Layout shared with the renderer's `FluidParticle`; size and offsets are asserted there.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParticleRecord {
    /// Scene-space metres; w = physical radius in metres. w = 0 marks an unused slot.
    pub position_radius: [f32; 4],
    /// Scene-space metres per second.
    pub velocity: [f32; 3],
    /// Birth order within `ParticleFrameInfo::identity_epoch`. 0 = no identity.
    pub id: u32,
}

const _: () = assert!(std::mem::size_of::<ParticleRecord>() == 32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParticleFrameInfo {
    pub count: u32,
    /// Changes when live ids are renumbered; frames from different epochs never match.
    pub identity_epoch: u32,
    /// Solid-distance node lattice written by this capture.
    pub solid_nodes: [u32; 3],
}

#[derive(Debug)]
pub enum CaptureError {
    /// Caller memory too small; nothing was published. Retry the same tick.
    Capacity { particles: u32, solid: usize },
    Fluid(FluidError),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity { particles, solid } => write!(
                formatter,
                "particle frame needs {particles} particle records and {solid} solid nodes"
            ),
            Self::Fluid(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<FluidError> for CaptureError {
    fn from(error: FluidError) -> Self {
        Self::Fluid(error)
    }
}

unsafe extern "C" {
    fn manifold_fluids_world_capture_particle_frame(
        world: *mut c_void,
        offset: *const f32,
        particles: *mut ParticleRecord,
        particle_capacity: usize,
        solid: *mut f32,
        solid_capacity: usize,
        count_out: *mut usize,
        nodes_out: *mut u32,
        fits_out: *mut i32,
    ) -> i32;
    #[cfg(feature = "face-oracle")]
    fn manifold_fluids_world_capture_face_v(world: *mut c_void, out: *mut f32, capacity: usize, dims_out: *mut u32) -> i32;
}

#[cfg(feature = "face-oracle")]
impl FluidWorld {
    /// After a completed step: the projected vertical face velocities, x
    /// fastest over `[isize, jsize + 1, ksize]`, returned with those dims.
    /// A probe oracle only.
    pub fn capture_face_v(&mut self, out: &mut Vec<f32>) -> Result<[u32; 3], FluidError> {
        let mut dims = [0u32; 3];
        // The entry reports its dims before it checks capacity.
        let mut ok = unsafe { manifold_fluids_world_capture_face_v(self.native, out.as_mut_ptr(), out.len(), dims.as_mut_ptr()) };
        let needed = dims.iter().map(|&d| d as usize).product::<usize>();
        if out.len() < needed {
            out.resize(needed, 0.0);
            ok = unsafe { manifold_fluids_world_capture_face_v(self.native, out.as_mut_ptr(), out.len(), dims.as_mut_ptr()) };
        }
        native_result(ok, "capturing vertical faces")?;
        Ok(dims)
    }
}

impl FluidWorld {
    /// After a completed step. Writes the marker particles, positions offset
    /// by `offset`, and the prepared solid distances the CPU mesher uses, in
    /// lattice order `i + nx·(j + ny·k)`. Never advances time.
    ///
    /// FLIP carries no particle identity: every record has id 0 and the
    /// identity epoch is 0. Records follow native storage order.
    pub fn capture_particle_frame(
        &mut self,
        offset: [f32; 3],
        particles: &mut [ParticleRecord],
        solid: &mut [f32],
    ) -> Result<ParticleFrameInfo, CaptureError> {
        let mut count = 0usize;
        let mut nodes = [0u32; 3];
        let mut fits = 0i32;
        let ok = unsafe {
            manifold_fluids_world_capture_particle_frame(
                self.native,
                offset.as_ptr(),
                particles.as_mut_ptr(),
                particles.len(),
                solid.as_mut_ptr(),
                solid.len(),
                &mut count,
                nodes.as_mut_ptr(),
                &mut fits,
            )
        };
        native_result(ok, "capturing a particle frame")?;
        let count = u32::try_from(count)
            .map_err(|_| FluidError::native("particle frame exceeds 32-bit particle indexing"))?;
        if fits == 0 {
            return Err(CaptureError::Capacity {
                particles: count,
                solid: nodes.iter().map(|&n| n as usize).product(),
            });
        }
        Ok(ParticleFrameInfo {
            count,
            identity_epoch: 0,
            solid_nodes: nodes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bounds, Config, Seconds, SurfaceOptions};

    unsafe extern "C" {
        fn manifold_fluids_surface_frame_solid(
            frame: *mut c_void,
            solid: *mut f32,
            capacity: usize,
            nodes_out: *mut u32,
        ) -> i32;
    }

    const CELLS: u32 = 12;
    const CELL_SIZE: f64 = 0.2;
    const SOLID_NODES: usize = ((CELLS + 1) * (CELLS + 1) * (CELLS + 1)) as usize;

    fn dam_break(seed: u64) -> FluidWorld {
        let mut world = FluidWorld::new_seeded(
            Config {
                cells: [CELLS; 3],
                cell_size: CELL_SIZE,
                surface_subdivisions: 0,
                apic: false,
            },
            seed,
        )
        .unwrap();
        world.set_gravity([0.0, -9.81, 0.0]).unwrap();
        world
            .add_fluid_box(
                Bounds {
                    min: [0.4, 0.4, 0.4],
                    max: [1.4, 2.0, 1.4],
                },
                [0.2, 0.0, 0.0],
            )
            .unwrap();
        world
    }

    fn capture(world: &mut FluidWorld, offset: [f32; 3]) -> (Vec<ParticleRecord>, Vec<f32>, ParticleFrameInfo) {
        let mut particles = vec![ParticleRecord::default(); 8192];
        let mut solid = vec![0.0; SOLID_NODES];
        let info = world
            .capture_particle_frame(offset, &mut particles, &mut solid)
            .unwrap();
        particles.truncate(info.count as usize);
        (particles, solid, info)
    }

    #[test]
    fn particle_frame_matches_marker_state() {
        let mut world = dam_break(3);
        let mut stats = Default::default();
        for _ in 0..6 {
            stats = world.step(Seconds(1.0 / 60.0)).unwrap();
        }
        let offset = [10.0, -2.0, 0.5];
        let (particles, _, info) = capture(&mut world, offset);
        assert_eq!(info.count, stats.particles);
        assert!(info.count > 1000);
        assert_eq!(info.identity_epoch, 0);
        assert_eq!(info.solid_nodes, [CELLS + 1; 3]);

        let mut mean_position = [0.0f32; 3];
        let mut mean_velocity = [0.0f32; 3];
        let ok = unsafe {
            crate::manifold_fluids_world_marker_motion(
                world.native,
                mean_position.as_mut_ptr(),
                mean_velocity.as_mut_ptr(),
            )
        };
        native_result(ok, "reading marker motion").unwrap();

        // Upstream marker volume is one eighth of a cell.
        let radius = (3.0 * CELL_SIZE.powi(3) / (32.0 * std::f64::consts::PI)).cbrt() as f32;
        let extent = CELLS as f32 * CELL_SIZE as f32;
        let mut sum_position = [0.0f64; 3];
        let mut sum_velocity = [0.0f64; 3];
        for record in &particles {
            assert_eq!(record.id, 0);
            assert!((record.position_radius[3] - radius).abs() < 1e-6);
            for axis in 0..3 {
                let native = record.position_radius[axis] - offset[axis];
                assert!((0.0..=extent).contains(&native), "{record:?}");
                sum_position[axis] += f64::from(native);
                sum_velocity[axis] += f64::from(record.velocity[axis]);
            }
        }
        for axis in 0..3 {
            let n = f64::from(info.count);
            assert!((sum_position[axis] / n - f64::from(mean_position[axis])).abs() < 1e-4);
            assert!((sum_velocity[axis] / n - f64::from(mean_velocity[axis])).abs() < 1e-4);
        }
        assert!(mean_velocity[1] < -0.1, "the column should be falling");
    }

    #[test]
    fn particle_frame_capture_leaves_solver_state_bit_identical() {
        let mut captured = dam_break(11);
        let mut untouched = dam_break(11);
        let mut particles = vec![ParticleRecord::default(); 8192];
        let mut solid = vec![0.0; SOLID_NODES];
        for _ in 0..8 {
            let a = captured.step(Seconds(1.0 / 60.0)).unwrap();
            let b = untouched.step(Seconds(1.0 / 60.0)).unwrap();
            assert_eq!(a.particles, b.particles);
            captured
                .capture_particle_frame([0.0; 3], &mut particles, &mut solid)
                .unwrap();
        }
        let mut expected = Vec::new();
        untouched.surface(&mut expected).unwrap();
        let mut actual = Vec::new();
        captured.surface(&mut actual).unwrap();
        assert_eq!(expected, actual);
        let (a, a_solid, _) = capture(&mut captured, [0.0; 3]);
        let (b, b_solid, _) = capture(&mut untouched, [0.0; 3]);
        assert_eq!(a, b);
        assert_eq!(
            a_solid.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b_solid.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn particle_frame_capacity_reports_required_counts() {
        let mut world = dam_break(5);
        world.step(Seconds(1.0 / 60.0)).unwrap();
        let (full, _, info) = capture(&mut world, [0.0; 3]);
        let sentinel = ParticleRecord {
            position_radius: [7.0; 4],
            velocity: [7.0; 3],
            id: 7,
        };
        for (particle_len, solid_len) in [
            (info.count as usize - 1, SOLID_NODES),
            (info.count as usize, SOLID_NODES - 1),
            (0, 0),
        ] {
            let mut particles = vec![sentinel; particle_len];
            let mut solid = vec![7.0; solid_len];
            match world.capture_particle_frame([0.0; 3], &mut particles, &mut solid) {
                Err(CaptureError::Capacity { particles: p, solid: s }) => {
                    assert_eq!(p, info.count);
                    assert_eq!(s, SOLID_NODES);
                }
                other => panic!("expected a capacity error, got {other:?}"),
            }
            assert!(particles.iter().all(|record| *record == sentinel));
            assert!(solid.iter().all(|value| *value == 7.0));
        }
        // The same tick is still capturable once the caller has room.
        let (again, _, _) = capture(&mut world, [0.0; 3]);
        assert_eq!(full, again);
    }

    #[test]
    fn particle_frame_solid_matches_surface_frame() {
        let mut world = dam_break(9);
        world.set_surface_options(SurfaceOptions::default()).unwrap();
        world
            .set_obstacle(
                Bounds { min: [1.4, 0.2, 1.0], max: [1.8, 1.2, 1.4] },
                Bounds { min: [1.41, 0.2, 1.0], max: [1.81, 1.2, 1.4] },
                Bounds { min: [1.42, 0.2, 1.0], max: [1.82, 1.2, 1.4] },
            )
            .unwrap();
        for _ in 0..3 {
            world.step(Seconds(1.0 / 60.0)).unwrap();
        }
        let (_, solid, info) = capture(&mut world, [0.0; 3]);
        let frame = world.capture_surface_frame().unwrap();
        let mut reference = vec![0.0f32; SOLID_NODES];
        let mut nodes = [0u32; 3];
        let ok = unsafe {
            manifold_fluids_surface_frame_solid(
                frame.native_ptr(),
                reference.as_mut_ptr(),
                reference.len(),
                nodes.as_mut_ptr(),
            )
        };
        native_result(ok, "reading the surface frame solid").unwrap();
        assert_eq!(nodes, info.solid_nodes);
        assert_eq!(
            solid.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            reference.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        assert!(solid.iter().any(|v| *v < 0.0), "the obstacle must be inside the lattice");
        // A second capture reuses its scratch and must repeat the lattice.
        let (_, repeated, _) = capture(&mut world, [0.0; 3]);
        assert_eq!(solid, repeated);
    }

    #[test]
    fn particle_frame_requires_a_completed_step() {
        let mut world = dam_break(1);
        let mut particles = vec![ParticleRecord::default(); 8192];
        let mut solid = vec![0.0; SOLID_NODES];
        assert!(matches!(
            world.capture_particle_frame([0.0; 3], &mut particles, &mut solid),
            Err(CaptureError::Fluid(_))
        ));
        drop(world.begin_frame(Seconds(1.0 / 60.0)).unwrap());
        assert!(matches!(
            world.capture_particle_frame([0.0; 3], &mut particles, &mut solid),
            Err(CaptureError::Fluid(_))
        ));
    }
}
