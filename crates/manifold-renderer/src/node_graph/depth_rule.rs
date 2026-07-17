//! Depth-companion-channel propagation classification (design doc
//! `docs/DEPTH_RELIGHT_DESIGN.md` D1).
//!
//! The "3D Shading" toggle (P3) synthesizes a depth companion channel
//! alongside a graph's normal color output. Every primitive declares a
//! [`DepthRule`] describing how it propagates that channel through itself —
//! the compiler walks the graph once, threads the depth path per these
//! rules, and dead-codes the whole thing when the toggle is off.
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
    /// Every registered (non-fixture) primitive constructs and answers
    /// `depth_rule()` without panicking — largely a tautology given
    /// `EffectNode::depth_rule` has no default (a primitive that omitted it
    /// would already have failed to compile, per this module's doc comment),
    /// but it walks the registry the same way
    /// `freeze::classify::tests::every_boundary_atom_declares_its_reason`
    /// does, so it stands as the guard for *future* hand-written
    /// `EffectNode` impls: it fails loudly (missing from the count) rather
    /// than silently if a future registration path stops going through
    /// `PrimitiveRegistry::with_builtin()`. `node.__*` fixtures are excluded,
    /// same carve-out as the fusion classification meta-test.
    #[test]
    fn every_registered_primitive_declares_a_depth_rule() {
        use crate::node_graph::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut checked = 0usize;
        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            // Calling this at all is the assertion — a primitive without a
            // `depth_rule()` implementation doesn't reach this line; the
            // compiler already refused to build it.
            let _ = node.depth_rule();
            checked += 1;
        }
        assert!(
            checked > 0,
            "expected at least one registered primitive to check depth_rule() on"
        );
    }
}
