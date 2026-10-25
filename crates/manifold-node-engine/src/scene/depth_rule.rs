//! Depth-companion-channel propagation classification.
//!
//! Every primitive declares a [`DepthRule`] describing how it would propagate
//! a depth companion channel through itself. No compiler pass reads it today;
//! it stays a required declaration so the classification remains truthful.
//!
//! Unlike [`FusionKind`](crate::node_graph::freeze::classify::FusionKind),
//! which defaults to `Boundary` when unset, `DepthRule` has **no default**:
//! every primitive must declare one explicitly, via the `primitive!` macro's
//! REQUIRED `depth_rule:` field (`PrimitiveSpec::DEPTH_RULE`) or directly on
//! a hand-written `EffectNode` impl. A primitive that doesn't declare it
//! fails to compile — there is no silent guess for the compiler to
//! propagate downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthRule {
    /// Pass the input's depth through untouched. Pure per-pixel color/tonal
    /// ops — anything whose output samples the SAME texel or a fixed
    /// neighborhood without UV remapping: tint, saturation, contrast,
    /// exposure, tone map, invert, gradient map, hue/saturation, chroma key,
    /// set alpha, scale/offset, smoothstep, convolutions, blurs.
    Inherit,
    /// Sample depth with the same UV transform as color. Mechanical: the
    /// kernel's resample expression applied to the depth channel.
    /// UV-remapping/resampling ops: transform, mirror, kaleidoscope-class,
    /// UV displace, texture advect, lens/distortion, feedback/temporal,
    /// downsample/resize-class.
    Warp,
    /// Multi-texture-input nodes: per-pixel nearest-depth wins (z-buffer
    /// semantics). `node.mix`'s Lerp mode lerps depth by the same amount.
    /// Multi-texture combiners: mix, hdr_mix, masked_mix, switch_texture,
    /// compose-class.
    CombineNearest,
    /// The node's output IS a height/scalar field: depth = own output
    /// luminance. Scalar-field/procedural texture producers (noise,
    /// simplex field, voronoi, polar field, SDF shapes, plasma-class curated
    /// kernels, LIC, height/pattern generators) and anything that renders
    /// geometry with real depth (render_3d_mesh-class, render_scene,
    /// draw_* splats, render_lines).
    SourceHeight,
    /// No meaningful depth (IO, bridges, control-rate). Chains ending with
    /// only `Terminal` producers have no depth origin; the toggle then uses
    /// D4's luminance-of-output fallback. Control-rate/scalar/array/IO/
    /// bridge/detection atoms with no Texture2D output, plus overlay/readout
    /// drawers whose output is annotation rather than content.
    Terminal,
}

#[cfg(test)]
mod tests {
}
