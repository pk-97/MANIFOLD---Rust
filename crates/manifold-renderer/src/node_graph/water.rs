//! Live Water — MLS-MPM records, constants and numerical helpers.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5. Field order and
//! std430 layout are load-bearing: shader structs mirror them exactly, and the
//! compile-time checks below are mandatory.

/// 96-byte particle record. `position_mass.w == 0.0` means the slot is inactive.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterParticle {
    pub position_mass: [f32; 4],     // world xyz (m); mass kg, zero = inactive
    pub velocity_density: [f32; 4],  // m/s xyz; density kg/m^3
    pub affine_x: [f32; 4],          // row 0 of C (1/s); w = 0
    pub affine_y: [f32; 4],          // row 1; w = 0
    pub affine_z: [f32; 4],          // row 2; w = 0
    pub previous_position: [f32; 4], // previous accepted substep xyz; w = 0
}

/// Resolved grid cell: velocity xyz (m/s) and mass (kg).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterGridCell {
    pub velocity_mass: [f32; 4],
}

const _: () = assert!(core::mem::size_of::<WaterParticle>() == 96);
const _: () = assert!(core::mem::size_of::<WaterGridCell>() == 16);

/// Fixed-point scale for grid mass/momentum accumulation (design section 5).
/// Sole encoding: no runtime fallback. S1 must prove quantisation error and
/// overflow headroom against the f64 reference before any GPU work.
pub const GRID_FIXED_SCALE: i32 = 1 << 20; // Q = 2^20

/// Sticky fault bits written by `water_validate`; cleared only by reset.
pub const FAULT_NONFINITE: u32 = 1;
pub const FAULT_INTEGER_OVERFLOW: u32 = 2;
pub const FAULT_OUTSIDE_DOMAIN: u32 = 4;
pub const FAULT_UNSUPPORTED_KINEMATICS: u32 = 8;
pub const FAULT_INVALID_DENSITY: u32 = 16;

#[cfg(test)]
mod reference;
