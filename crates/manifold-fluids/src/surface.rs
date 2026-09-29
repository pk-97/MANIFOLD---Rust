//! Explicit, owned snapshots for remeshing without another solver step.
//!
//! This is the single-frame boundary, not a persistent cache format. Capture
//! copies particles and the prepared collision distance field on the owner's
//! worker. Normal live simulation does not pay that copy or retain snapshots.

use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;

use super::{FluidError, FluidWorld, SurfaceOptions, SurfaceVertex, decode_surface, native_result};

unsafe extern "C" {
    fn manifold_fluids_world_capture_surface_frame(
        world: *mut c_void,
        frame: *mut *mut c_void,
    ) -> i32;
    fn manifold_fluids_surface_frame_destroy(frame: *mut c_void);
    fn manifold_fluids_surface_frame_mesh(
        frame: *mut c_void,
        subdivisions: u32,
        particle_scale: f64,
        smoothing: f64,
        iterations: u32,
        isolated_scale: f64,
        data: *mut *const u8,
        len: *mut usize,
    ) -> i32;
}

/// Frozen liquid positions and collision context from one completed frame.
///
/// Owns its inputs independently of the world. Repeated reconstruction starts
/// from those inputs, so changing smoothing never compounds a previous result.
/// It can outlive its world and move to a meshing worker, but is not shared.
pub struct SurfaceFrame {
    native: *mut c_void,
    vertices: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
    normals: Vec<[f32; 3]>,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: the pointer uniquely owns a native snapshot with no world pointers or
// thread affinity. All operations require exclusive access and use the bridge's
// existing native serialization; Cell prevents concurrent shared access.
unsafe impl Send for SurfaceFrame {}

impl FluidWorld {
    /// Copy reconstruction inputs after a completed step. This does not advance
    /// time, change the current mesh, or retain a reference to this world.
    pub fn capture_surface_frame(&mut self) -> Result<SurfaceFrame, FluidError> {
        let mut native = std::ptr::null_mut();
        let ok = unsafe { manifold_fluids_world_capture_surface_frame(self.native, &mut native) };
        native_result(ok, "capturing surface reconstruction inputs")?;
        if native.is_null() {
            return Err(FluidError::native("native surface frame pointer is null"));
        }
        Ok(SurfaceFrame {
            native,
            vertices: Vec::new(),
            triangles: Vec::new(),
            normals: Vec::new(),
            _not_sync: PhantomData,
        })
    }
}

impl SurfaceFrame {
    /// Reconstruct with the production particle mesher and normal decoder.
    /// `subdivisions` has the same 0..=2 meaning as Surface Detail in the app.
    /// This performs CPU meshing work, with no solver or whitewater update.
    pub fn reconstruct(
        &mut self,
        subdivisions: u32,
        options: SurfaceOptions,
        output: &mut Vec<SurfaceVertex>,
    ) -> Result<(), FluidError> {
        self.reconstruct_with_isolated_scale(subdivisions, options, 1.0, output)
    }

    /// Adjust single isolated particles while preserving overlapping particle
    /// radii. `isolated_scale` is a fraction of the main reconstruction radius,
    /// in 0.25..=1. A smooth distance transition avoids a binary size switch.
    /// It is independent of mesh detail and scales with the simulation grid.
    /// Small droplets can disappear below the selected meshing resolution.
    /// This experimental control is not yet exposed in the application.
    pub fn reconstruct_with_isolated_scale(
        &mut self,
        subdivisions: u32,
        options: SurfaceOptions,
        isolated_scale: f64,
        output: &mut Vec<SurfaceVertex>,
    ) -> Result<(), FluidError> {
        options.validate()?;
        if !isolated_scale.is_finite() || !(0.25..=1.0).contains(&isolated_scale) {
            return Err(FluidError::input(
                "isolated particle scale must be finite and in 0.25..=1",
            ));
        }
        if subdivisions > 2 {
            return Err(FluidError::input("surface subdivisions must be in 0..=2"));
        }
        let mut data = std::ptr::null();
        let mut len = 0;
        let ok = unsafe {
            manifold_fluids_surface_frame_mesh(
                self.native,
                subdivisions,
                options.particle_scale,
                options.smoothing,
                options.smoothing_iterations,
                isolated_scale,
                &mut data,
                &mut len,
            )
        };
        native_result(ok, "reconstructing a captured surface")?;
        let bytes = if len == 0 {
            &[]
        } else if data.is_null() {
            return Err(FluidError::native(
                "native surface frame returned null mesh data",
            ));
        } else {
            // SAFETY: the buffer is owned by this exclusively borrowed snapshot
            // and remains valid until its next native operation or destruction.
            unsafe { std::slice::from_raw_parts(data, len) }
        };
        decode_surface(
            bytes,
            output,
            &mut self.vertices,
            &mut self.triangles,
            &mut self.normals,
        )
    }
}

impl Drop for SurfaceFrame {
    fn drop(&mut self) {
        unsafe { manifold_fluids_surface_frame_destroy(self.native) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bounds, Config, Seconds};

    unsafe extern "C" {
        fn manifold_fluids_surface_frame_fixture(
            scale: f64,
            chunks: u32,
            frame: *mut *mut c_void,
        ) -> i32;
    }

    fn fixture(scale: f64, chunks: u32) -> SurfaceFrame {
        let mut native = std::ptr::null_mut();
        let ok = unsafe { manifold_fluids_surface_frame_fixture(scale, chunks, &mut native) };
        native_result(ok, "creating bounded meshing fixture").unwrap();
        assert!(!native.is_null());
        SurfaceFrame {
            native,
            vertices: Vec::new(),
            triangles: Vec::new(),
            normals: Vec::new(),
            _not_sync: PhantomData,
        }
    }

    #[test]
    fn surface_frame_isolated_radius_preserves_dense_surface_and_scales_with_cells() {
        let options = SurfaceOptions {
            particle_scale: 1.0,
            smoothing: 0.0,
            smoothing_iterations: 0,
        };
        let mut reference = Vec::new();
        for scale in [1.0, 0.5, 2.0] {
            let mut frame = fixture(scale, 3);
            let mut original = Vec::new();
            frame.reconstruct(2, options, &mut original).unwrap();
            let mut shrunk = Vec::new();
            frame
                .reconstruct_with_isolated_scale(2, options, 0.6, &mut shrunk)
                .unwrap();
            let dense = |mesh: &[SurfaceVertex]| {
                mesh.iter()
                    .copied()
                    .filter(|v| v.position[0] < 1.5 * scale as f32)
                    .collect::<Vec<_>>()
            };
            assert!(!dense(&original).is_empty());
            assert_same_surface(&dense(&original), &dense(&shrunk));
            for centre in [2.03, 2.73] {
                let width = |mesh: &[SurfaceVertex]| {
                    let xs: Vec<_> = mesh
                        .iter()
                        .map(|v| v.position[0] / scale as f32)
                        .filter(|x| (*x - centre).abs() < 0.25)
                        .collect();
                    assert!(!xs.is_empty(), "isolated droplet disappeared");
                    xs.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                        - xs.iter().copied().fold(f32::INFINITY, f32::min)
                };
                assert!(width(&shrunk) < width(&original) * 0.75);
            }
            // Tuning either radius must invalidate the right retained inputs.
            let mut retuned = Vec::new();
            frame
                .reconstruct_with_isolated_scale(2, options, 0.85, &mut retuned)
                .unwrap();
            frame
                .reconstruct_with_isolated_scale(2, options, 0.6, &mut retuned)
                .unwrap();
            assert_same_surface(&shrunk, &retuned);
            frame
                .reconstruct_with_isolated_scale(
                    2,
                    SurfaceOptions {
                        particle_scale: 0.8,
                        ..options
                    },
                    0.6,
                    &mut retuned,
                )
                .unwrap();
            frame
                .reconstruct_with_isolated_scale(2, options, 0.6, &mut retuned)
                .unwrap();
            assert_same_surface(&shrunk, &retuned);
            for vertex in &mut shrunk {
                for coordinate in &mut vertex.position {
                    *coordinate /= scale as f32;
                }
            }
            if reference.is_empty() {
                reference = shrunk.clone();
            } else {
                assert_same_surface(&reference, &shrunk);
            }
            frame.reconstruct(2, options, &mut shrunk).unwrap();
            assert_same_surface(&original, &shrunk);
            for invalid in [f64::NAN, 0.0, 0.24, 1.01] {
                assert!(
                    frame
                        .reconstruct_with_isolated_scale(2, options, invalid, &mut shrunk)
                        .is_err()
                );
            }
        }
    }

    fn world() -> FluidWorld {
        FluidWorld::new(Config {
            cells: [12; 3],
            cell_size: 0.2,
            surface_subdivisions: 0,
            apic: false,
        })
        .unwrap()
    }

    fn assert_same_surface(expected: &[SurfaceVertex], actual: &[SurfaceVertex]) {
        assert_eq!(expected.len(), actual.len());
        for (a, b) in expected.iter().zip(actual) {
            for (x, y) in a
                .position
                .iter()
                .chain(&a.normal)
                .zip(b.position.iter().chain(&b.normal))
            {
                assert!((x - y).abs() < 2e-6, "surface changed: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn surface_frame_reconstructs_production_mesh_and_outlives_moving_obstacle_world() {
        let mut world = world();
        let options = SurfaceOptions::default();
        world.set_surface_options(options).unwrap();
        world
            .add_fluid_box(
                Bounds {
                    min: [0.5, 0.5, 0.5],
                    max: [1.9, 1.3, 1.9],
                },
                [0.5, 0.0, 0.0],
            )
            .unwrap();
        let previous = Bounds {
            min: [0.8, 0.4, 0.8],
            max: [1.2, 1.5, 1.2],
        };
        let current = Bounds {
            min: [0.81, 0.4, 0.8],
            max: [1.21, 1.5, 1.2],
        };
        let next = Bounds {
            min: [0.82, 0.4, 0.8],
            max: [1.22, 1.5, 1.2],
        };
        world.set_obstacle(previous, current, next).unwrap();
        world.step(Seconds(1.0 / 60.0)).unwrap();
        let mut original = Vec::new();
        world.surface(&mut original).unwrap();
        assert!(!original.is_empty());
        let mut frame = world.capture_surface_frame().unwrap();
        let mut output = Vec::new();
        frame.reconstruct(0, options, &mut output).unwrap();
        assert_same_surface(&original, &output);
        let mut unchanged = Vec::new();
        world.surface(&mut unchanged).unwrap();
        assert_same_surface(&original, &unchanged);

        world.clear_obstacle().unwrap();
        world.step(Seconds(1.0 / 60.0)).unwrap();
        drop(world);
        // A solver cannot be called here: its entire owner has been destroyed.
        frame
            .reconstruct(
                1,
                SurfaceOptions {
                    particle_scale: 1.5,
                    smoothing: 0.8,
                    smoothing_iterations: 4,
                },
                &mut output,
            )
            .unwrap();
        assert!(!output.is_empty());
        assert_ne!(original, output);
        assert!(
            output
                .iter()
                .all(|v| v.position.iter().chain(&v.normal).all(|x| x.is_finite()))
        );
        frame.reconstruct(0, options, &mut output).unwrap();
        assert_same_surface(&original, &output);
    }

    #[test]
    fn surface_frame_empty_and_invalid_inputs_are_explicit() {
        let mut world = world();
        assert!(world.capture_surface_frame().is_err());
        world.step(Seconds(1.0 / 60.0)).unwrap();
        let mut frame = world.capture_surface_frame().unwrap();
        let mut output = vec![SurfaceVertex::default()];
        frame
            .reconstruct(0, SurfaceOptions::default(), &mut output)
            .unwrap();
        assert!(output.is_empty());
        assert!(
            frame
                .reconstruct(3, SurfaceOptions::default(), &mut output)
                .is_err()
        );
        assert!(
            frame
                .reconstruct(
                    0,
                    SurfaceOptions {
                        particle_scale: f64::NAN,
                        ..SurfaceOptions::default()
                    },
                    &mut output
                )
                .is_err()
        );
        drop(world.begin_frame(Seconds(1.0 / 60.0)).unwrap());
        assert!(world.capture_surface_frame().is_err());
    }
}
