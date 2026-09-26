//! Safe, owned access to the pinned FLIP Fluids CPU reference engine.
//!
//! The upstream engine keeps mutable process-global thread and source state. The
//! bridge serializes every native operation, so worlds can move to worker threads
//! but cannot be shared concurrently. This is deliberately a CPU-reference
//! limitation; surface decoding itself happens after the native call and does
//! not hold that global lock.

use manifold_foundation::Seconds;
use std::cell::Cell;
use std::ffi::CStr;
use std::fmt;
use std::marker::PhantomData;

pub const UPSTREAM_REVISION: &str = "70a0e954018fe39e1f9c3631264989569752bb7a";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub cells: [u32; 3],
    pub cell_size: f64,
    pub surface_subdivisions: u32,
    pub apic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SurfaceVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub particles: u32,
    pub triangles: u32,
    pub substeps: u32,
    /// Upstream total update time, including surface meshing.
    pub simulation_ms: f64,
    pub meshing_ms: f64,
}

/// An actionable error from the native FLIP bridge or its input boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FluidError {
    message: String,
}

impl FluidError {
    fn input(message: impl Into<String>) -> Self {
        Self {
            message: format!("invalid fluid input: {}", message.into()),
        }
    }

    fn native(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FluidError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FluidError {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NativeFrameStats {
    particles: u32,
    triangles: u32,
    substeps: u32,
    simulation_ms: f64,
    meshing_ms: f64,
}

unsafe extern "C" {
    fn manifold_fluids_world_create(
        isize: u32,
        jsize: u32,
        ksize: u32,
        cell_size: f64,
        surface_subdivisions: u32,
        apic: i32,
        world_out: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn manifold_fluids_world_destroy(world: *mut std::ffi::c_void);
    fn manifold_fluids_world_add_fluid_box(
        world: *mut std::ffi::c_void,
        min: *const f32,
        max: *const f32,
        velocity: *const f32,
    ) -> i32;
    fn manifold_fluids_world_set_gravity(world: *mut std::ffi::c_void, gravity: *const f32) -> i32;
    fn manifold_fluids_world_set_emitter(
        world: *mut std::ffi::c_void,
        min: *const f32,
        max: *const f32,
        velocity: *const f32,
        enabled: i32,
    ) -> i32;
    fn manifold_fluids_world_set_obstacle(
        world: *mut std::ffi::c_void,
        previous_min: *const f32,
        previous_max: *const f32,
        current_min: *const f32,
        current_max: *const f32,
        next_min: *const f32,
        next_max: *const f32,
    ) -> i32;
    fn manifold_fluids_world_step(
        world: *mut std::ffi::c_void,
        dt: f64,
        stats_out: *mut NativeFrameStats,
    ) -> i32;
    fn manifold_fluids_world_surface(
        world: *mut std::ffi::c_void,
        data_out: *mut *const u8,
        len_out: *mut usize,
    ) -> i32;
    fn manifold_fluids_last_error() -> *const std::ffi::c_char;
}

/// An exclusively owned native FLIP simulation world.
pub struct FluidWorld {
    native: *mut std::ffi::c_void,
    vertex_scratch: Vec<[f32; 3]>,
    triangle_scratch: Vec<[u32; 3]>,
    normal_scratch: Vec<[f32; 3]>,
    // Cell is Send but not Sync, matching exclusive world ownership.
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: the pointer owns one world, all access requires &mut self, and the
// bridge serializes upstream process-global state. Native objects have no
// thread affinity. Cell keeps the wrapper !Sync.
unsafe impl Send for FluidWorld {}

impl FluidWorld {
    pub fn new(config: Config) -> Result<Self, FluidError> {
        validate_config(config)?;
        let mut native = std::ptr::null_mut();
        let ok = unsafe {
            manifold_fluids_world_create(
                config.cells[0],
                config.cells[1],
                config.cells[2],
                config.cell_size,
                config.surface_subdivisions,
                i32::from(config.apic),
                &mut native,
            )
        };
        if ok == 0 || native.is_null() {
            return Err(last_native_error("FLIP Fluids world creation failed"));
        }
        Ok(Self {
            native,
            vertex_scratch: Vec::new(),
            triangle_scratch: Vec::new(),
            normal_scratch: Vec::new(),
            _not_sync: PhantomData,
        })
    }

    pub fn add_fluid_box(&mut self, bounds: Bounds, velocity: [f32; 3]) -> Result<(), FluidError> {
        validate_bounds(bounds)?;
        validate_vector(velocity, "fluid velocity")?;
        let ok = unsafe {
            manifold_fluids_world_add_fluid_box(
                self.native,
                bounds.min.as_ptr(),
                bounds.max.as_ptr(),
                velocity.as_ptr(),
            )
        };
        native_result(ok, "adding a fluid box")
    }

    pub fn set_gravity(&mut self, gravity: [f32; 3]) -> Result<(), FluidError> {
        validate_vector(gravity, "gravity")?;
        let ok = unsafe { manifold_fluids_world_set_gravity(self.native, gravity.as_ptr()) };
        native_result(ok, "setting gravity")
    }

    pub fn set_emitter(
        &mut self,
        bounds: Bounds,
        velocity: [f32; 3],
        enabled: bool,
    ) -> Result<(), FluidError> {
        validate_bounds(bounds)?;
        validate_vector(velocity, "emitter velocity")?;
        let ok = unsafe {
            manifold_fluids_world_set_emitter(
                self.native,
                bounds.min.as_ptr(),
                bounds.max.as_ptr(),
                velocity.as_ptr(),
                i32::from(enabled),
            )
        };
        native_result(ok, "setting the fluid emitter")
    }

    pub fn set_obstacle(
        &mut self,
        previous: Bounds,
        current: Bounds,
        next: Bounds,
    ) -> Result<(), FluidError> {
        validate_bounds(previous)?;
        validate_bounds(current)?;
        validate_bounds(next)?;
        let ok = unsafe {
            manifold_fluids_world_set_obstacle(
                self.native,
                previous.min.as_ptr(),
                previous.max.as_ptr(),
                current.min.as_ptr(),
                current.max.as_ptr(),
                next.min.as_ptr(),
                next.max.as_ptr(),
            )
        };
        native_result(ok, "setting the fluid obstacle")
    }

    pub fn step(&mut self, dt: Seconds) -> Result<FrameStats, FluidError> {
        if !(dt.0.is_finite() && dt.0 > 0.0 && dt.0 <= 1.0 / 30.0) {
            return Err(FluidError::input(
                "dt must be finite and in (0, 1/30] seconds",
            ));
        }
        let mut native_stats = NativeFrameStats::default();
        let ok = unsafe { manifold_fluids_world_step(self.native, dt.0, &mut native_stats) };
        native_result(ok, "stepping the fluid world")?;
        Ok(FrameStats {
            particles: native_stats.particles,
            triangles: native_stats.triangles,
            substeps: native_stats.substeps,
            simulation_ms: native_stats.simulation_ms,
            meshing_ms: native_stats.meshing_ms,
        })
    }

    pub fn surface(&mut self, output: &mut Vec<SurfaceVertex>) -> Result<(), FluidError> {
        let mut data = std::ptr::null();
        let mut len = 0;
        let ok = unsafe { manifold_fluids_world_surface(self.native, &mut data, &mut len) };
        native_result(ok, "reading the fluid surface")?;
        let bytes = if len == 0 {
            &[]
        } else if data.is_null() {
            return Err(FluidError::native(
                "FLIP Fluids returned a null surface buffer with nonzero length",
            ));
        } else {
            unsafe { std::slice::from_raw_parts(data, len) }
        };
        decode_surface(
            bytes,
            output,
            &mut self.vertex_scratch,
            &mut self.triangle_scratch,
            &mut self.normal_scratch,
        )
    }
}

impl Drop for FluidWorld {
    fn drop(&mut self) {
        if !self.native.is_null() {
            unsafe { manifold_fluids_world_destroy(self.native) };
            self.native = std::ptr::null_mut();
        }
    }
}

fn validate_config(config: Config) -> Result<(), FluidError> {
    if !config.cell_size.is_finite() || config.cell_size <= 0.0 {
        return Err(FluidError::input("cell_size must be finite and positive"));
    }
    if config
        .cells
        .iter()
        .any(|&cells| !(8..=128).contains(&cells))
    {
        return Err(FluidError::input("each cell count must be in 8..=128"));
    }
    let total = u64::from(config.cells[0])
        .checked_mul(u64::from(config.cells[1]))
        .and_then(|value| value.checked_mul(u64::from(config.cells[2])))
        .ok_or_else(|| FluidError::input("cell count overflow"))?;
    if total > 128_u64.pow(3) {
        return Err(FluidError::input("total cell count must be at most 128^3"));
    }
    if config.surface_subdivisions > 2 {
        return Err(FluidError::input("surface_subdivisions must be in 0..=2"));
    }
    Ok(())
}

fn validate_bounds(bounds: Bounds) -> Result<(), FluidError> {
    for axis in 0..3 {
        if !bounds.min[axis].is_finite()
            || !bounds.max[axis].is_finite()
            || bounds.min[axis] >= bounds.max[axis]
        {
            return Err(FluidError::input(
                "bounds must be finite and have positive extent on every axis",
            ));
        }
    }
    Ok(())
}

fn validate_vector(vector: [f32; 3], name: &str) -> Result<(), FluidError> {
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(FluidError::input(format!("{name} must be finite")));
    }
    Ok(())
}

fn native_result(ok: i32, operation: &str) -> Result<(), FluidError> {
    if ok != 0 {
        Ok(())
    } else {
        Err(last_native_error(operation))
    }
}

fn last_native_error(operation: &str) -> FluidError {
    let message = unsafe {
        let pointer = manifold_fluids_last_error();
        if pointer.is_null() {
            None
        } else {
            Some(CStr::from_ptr(pointer).to_string_lossy().into_owned())
        }
    };
    FluidError::native(match message {
        Some(message) if !message.is_empty() => format!("{operation}: {message}"),
        _ => operation.to_owned(),
    })
}

fn decode_surface(
    bytes: &[u8],
    output: &mut Vec<SurfaceVertex>,
    vertices: &mut Vec<[f32; 3]>,
    triangles: &mut Vec<[u32; 3]>,
    normals: &mut Vec<[f32; 3]>,
) -> Result<(), FluidError> {
    output.clear();
    vertices.clear();
    triangles.clear();
    normals.clear();
    if bytes.is_empty() {
        return Ok(());
    }
    let mut cursor = 0usize;
    let vertex_count = read_i32(bytes, &mut cursor, "vertex count")?;
    if vertex_count < 0 {
        return Err(FluidError::native("surface vertex count is negative"));
    }
    let vertex_count = usize::try_from(vertex_count)
        .map_err(|_| FluidError::native("surface vertex count does not fit usize"))?;
    let vertex_bytes = vertex_count
        .checked_mul(3)
        .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
        .ok_or_else(|| FluidError::native("surface vertex data length overflow"))?;
    if bytes.len().saturating_sub(cursor) < vertex_bytes {
        return Err(FluidError::native("surface vertex data is truncated"));
    }
    vertices.reserve(vertex_count);
    for _ in 0..vertex_count {
        let position = [
            read_f32(bytes, &mut cursor, "surface vertex")?,
            read_f32(bytes, &mut cursor, "surface vertex")?,
            read_f32(bytes, &mut cursor, "surface vertex")?,
        ];
        if position.iter().any(|value| !value.is_finite()) {
            return Err(FluidError::native(
                "surface vertex contains a non-finite value",
            ));
        }
        vertices.push(position);
    }
    let triangle_count = read_i32(bytes, &mut cursor, "triangle count")?;
    if triangle_count < 0 {
        return Err(FluidError::native("surface triangle count is negative"));
    }
    let triangle_count = usize::try_from(triangle_count)
        .map_err(|_| FluidError::native("surface triangle count does not fit usize"))?;
    let triangle_bytes = triangle_count
        .checked_mul(3)
        .and_then(|count| count.checked_mul(std::mem::size_of::<i32>()))
        .ok_or_else(|| FluidError::native("surface triangle data length overflow"))?;
    if bytes.len().saturating_sub(cursor) < triangle_bytes {
        return Err(FluidError::native("surface triangle data is truncated"));
    }
    triangles.reserve(triangle_count);
    normals.resize(vertex_count, [0.0; 3]);
    for _ in 0..triangle_count {
        let indices = [
            read_i32(bytes, &mut cursor, "surface index")?,
            read_i32(bytes, &mut cursor, "surface index")?,
            read_i32(bytes, &mut cursor, "surface index")?,
        ];
        if indices.iter().any(|&index| index < 0) {
            return Err(FluidError::native("surface index is negative"));
        }
        let indices = [indices[0] as u32, indices[1] as u32, indices[2] as u32];
        if indices.iter().any(|&index| index as usize >= vertex_count) {
            return Err(FluidError::native("surface index is out of range"));
        }
        let a = vertices[indices[0] as usize];
        let b = vertices[indices[1] as usize];
        let c = vertices[indices[2] as usize];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        for &index in &indices {
            let accumulated = &mut normals[index as usize];
            accumulated[0] += normal[0];
            accumulated[1] += normal[1];
            accumulated[2] += normal[2];
        }
        triangles.push(indices);
    }
    if cursor != bytes.len() {
        return Err(FluidError::native("surface data has trailing bytes"));
    }
    output.reserve(triangle_count.saturating_mul(3));
    for indices in triangles.iter().copied() {
        for index in indices {
            let normal = normalize_normal(normals[index as usize]);
            output.push(SurfaceVertex {
                position: vertices[index as usize],
                normal,
            });
        }
    }
    Ok(())
}

fn read_i32(bytes: &[u8], cursor: &mut usize, name: &str) -> Result<i32, FluidError> {
    let end = cursor
        .checked_add(4)
        .ok_or_else(|| FluidError::native(format!("{name} offset overflow")))?;
    let chunk = bytes
        .get(*cursor..end)
        .ok_or_else(|| FluidError::native(format!("{name} is truncated")))?;
    *cursor = end;
    Ok(i32::from_ne_bytes(
        chunk.try_into().expect("four-byte slice"),
    ))
}

fn read_f32(bytes: &[u8], cursor: &mut usize, name: &str) -> Result<f32, FluidError> {
    let end = cursor
        .checked_add(4)
        .ok_or_else(|| FluidError::native(format!("{name} offset overflow")))?;
    let chunk = bytes
        .get(*cursor..end)
        .ok_or_else(|| FluidError::native(format!("{name} is truncated")))?;
    *cursor = end;
    Ok(f32::from_ne_bytes(
        chunk.try_into().expect("four-byte slice"),
    ))
}

fn normalize_normal(normal: [f32; 3]) -> [f32; 3] {
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if length > f32::EPSILON && length.is_finite() {
        [normal[0] / length, normal[1] / length, normal[2] / length]
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::{Bounds, Config, Seconds, SurfaceVertex, decode_surface};

    fn bobj(vertices: &[[f32; 3]], triangles: &[[i32; 3]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(vertices.len() as i32).to_ne_bytes());
        for vertex in vertices {
            for value in vertex {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        bytes.extend_from_slice(&(triangles.len() as i32).to_ne_bytes());
        for triangle in triangles {
            for value in triangle {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        bytes
    }

    #[test]
    fn decodes_deindexed_surface_with_smooth_normals() {
        let bytes = bobj(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            &[[0, 1, 2]],
        );
        let mut output = Vec::<SurfaceVertex>::new();
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        let mut normals = Vec::new();
        decode_surface(
            &bytes,
            &mut output,
            &mut vertices,
            &mut triangles,
            &mut normals,
        )
        .expect("valid BOBJ");
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(output[1].position, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn rejects_truncated_and_out_of_range_surface_data() {
        let mut output = Vec::new();
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        let mut normals = Vec::new();
        assert!(
            decode_surface(
                &[1, 0, 0, 0],
                &mut output,
                &mut vertices,
                &mut triangles,
                &mut normals
            )
            .is_err()
        );
        let bytes = bobj(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            &[[0, 1, 4]],
        );
        assert!(
            decode_surface(
                &bytes,
                &mut output,
                &mut vertices,
                &mut triangles,
                &mut normals
            )
            .is_err()
        );
    }

    #[test]
    fn validates_config_and_bounds_before_native_calls() {
        assert!(
            super::validate_config(Config {
                cells: [7, 16, 16],
                cell_size: 0.1,
                surface_subdivisions: 0,
                apic: false,
            })
            .is_err()
        );
        assert!(
            super::validate_bounds(Bounds {
                min: [0.0, 0.0, 0.0],
                max: [1.0, f32::NAN, 1.0],
            })
            .is_err()
        );
    }

    #[test]
    fn runs_tiny_native_scene_with_inflow_gravity_and_moving_obstacle() {
        let mut world = super::FluidWorld::new(Config {
            cells: [16, 16, 16],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.5, 0.5, 0.5],
                    max: [2.5, 2.0, 2.5],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("fluid box");
        world
            .set_emitter(
                Bounds {
                    min: [1.0, 2.0, 1.0],
                    max: [1.5, 2.25, 1.5],
                },
                [0.0, -0.5, 0.0],
                true,
            )
            .expect("emitter");
        world
            .set_obstacle(
                Bounds {
                    min: [1.0, 0.0, 1.0],
                    max: [1.5, 0.5, 1.5],
                },
                Bounds {
                    min: [1.05, 0.0, 1.0],
                    max: [1.55, 0.5, 1.5],
                },
                Bounds {
                    min: [1.1, 0.0, 1.0],
                    max: [1.6, 0.5, 1.5],
                },
            )
            .expect("obstacle");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("native step");
        assert!(stats.particles > 0, "tiny scene emitted no particles");
        let mut surface = Vec::new();
        world.surface(&mut surface).expect("native surface");
        assert!(!surface.is_empty(), "tiny scene produced no surface");
        assert!(surface.iter().all(|vertex| {
            vertex
                .position
                .iter()
                .chain(vertex.normal.iter())
                .all(|value| value.is_finite())
        }));
    }

    #[test]
    fn empty_native_domain_is_a_valid_empty_output() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: true,
        })
        .expect("native world");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("empty step");
        assert_eq!(stats.particles, 0);
        let mut surface = vec![SurfaceVertex::default()];
        world.surface(&mut surface).expect("empty surface");
        assert!(surface.is_empty());
    }
}
