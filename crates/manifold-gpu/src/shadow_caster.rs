//! The shadow-caster record both renderers read: the raster samples its
//! shadow map with it, the ray tracer traces its shadow rays from it. Backend
//! neutral: plain f32 vec4/mat4 fields, with the Metal and WGSL struct text
//! generated from the Rust declaration.

use crate::BufferUse;
use bytemuck::{Pod, Zeroable};

/// Declares a GPU record once: the Rust struct plus the matching MSL and WGSL
/// struct text. Every field is a `vec4` or `mat4` of f32, so the layout is the
/// same in Rust, Metal and WGSL with no padding rules to keep in step.
macro_rules! gpu_record {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident : $ty:ident),* $(,)? }) => {
        $(#[$meta])*
        #[repr(C)]
        #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
        pub struct $name { $($(#[$fmeta])* pub $field: gpu_record!(@rust $ty),)* }
        impl $name {
            /// The Metal struct, generated from the Rust fields. Types are qualified
            /// because it is placed ahead of the kernel source's includes.
            pub const MSL: &'static str = concat!(
                "struct ", stringify!($name), " {\n",
                $("    ", gpu_record!(@msl $ty), " ", stringify!($field), ";\n",)*
                "};\n");
            /// The WGSL struct, generated from the Rust fields. WGSL files
            /// carry a verbatim copy; a test checks the copy matches.
            pub const WGSL: &'static str = concat!(
                "struct ", stringify!($name), " {\n",
                $("    ", stringify!($field), ": ", gpu_record!(@wgsl $ty), ",\n",)*
                "}\n");
        }
    };
    (@rust vec4) => { [f32; 4] };
    (@rust mat4) => { [[f32; 4]; 4] };
    (@msl vec4) => { "metal::float4" };
    (@msl mat4) => { "metal::float4x4" };
    (@wgsl vec4) => { "vec4<f32>" };
    (@wgsl mat4) => { "mat4x4<f32>" };
}

gpu_record! {
    /// One shadow-casting light, read by both renderers. The raster reads
    /// `view_proj` and `shadow` to sample its shadow map; the ray tracer reads
    /// `source` and `color` to trace its shadow rays. One table of these, one
    /// entry per shadow view in light order (a point light's six cube faces
    /// are six), sits after the light table in the same per-frame buffer.
    /// Entry `i` is caster slot `i`: its shadow-map layer, and channel `i % 4`
    /// of shadow-visibility layer `i / 4`.
    ShadowCaster {
        /// The light's view-projection for its shadow map (column-major).
        view_proj: mat4,
        /// Shadow bias, filter kernel half-width in texels (negative = the
        /// Contact tier's blocker search), texel size, Contact light size.
        shadow: vec4,
        /// `xyz`: a sun's direction toward the light, or a point or spot
        /// light's position. `w`: a sun's cone half-angle in radians, or a
        /// point light's diameter (0 = hard shadow).
        source: vec4,
        /// `rgb`: colour times intensity. `w`: the kind, [`Self::SUN`],
        /// [`Self::POSITIONAL`], [`Self::CUBE`] or [`Self::CUBE_FACE`].
        color: vec4,
    }
}

impl ShadowCaster {
    /// A sun: `source` is its direction. This slot is its nearest cascade
    /// and the next three its farther ones (`light::CASCADES`).
    pub const SUN: f32 = 0.0;
    /// A point or spot light with one shadow view: `source` is its position.
    pub const POSITIONAL: f32 = 1.0;
    /// A point light's cube: this slot is its +X face and the next five its
    /// -X, +Y, -Y, +Z and -Z faces. Traced like [`Self::POSITIONAL`].
    pub const CUBE: f32 = 2.0;
    /// One of a cube's five later faces or a sun's later cascades: a shadow
    /// view only, black, so the tracer shadows the light once, from its
    /// first slot.
    pub const CUBE_FACE: f32 = 3.0;

    /// A ray-traced caster with no shadow map, for tracer-only callers.
    /// `kind` 0 is a sun, 1 a point light.
    pub fn traced(dir_or_pos: [f32; 3], cone_or_size: f32, color: [f32; 3], kind: u32) -> Self {
        Self {
            source: [dir_or_pos[0], dir_or_pos[1], dir_or_pos[2], cone_or_size],
            color: [color[0], color[1], color[2], kind as f32],
            ..Self::default()
        }
    }

    /// True for a sun caster.
    pub fn is_sun(&self) -> bool {
        self.color[3] == Self::SUN
    }
}

/// Upload a caster table into a fresh shared buffer. For proofs and one-off
/// callers; the renderer writes its table into its per-frame light ring.
pub fn upload_shadow_casters(
    device: &crate::GpuDevice,
    casters: &[ShadowCaster],
) -> crate::GpuBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(casters);
    let buffer = device.create_buffer(
        bytes.len().max(std::mem::size_of::<ShadowCaster>()) as u64,
        BufferUse::Upload,
    );
    // SAFETY: the buffer was just created with room for `bytes`.
    unsafe { buffer.write(0, bytes) };
    buffer
}
