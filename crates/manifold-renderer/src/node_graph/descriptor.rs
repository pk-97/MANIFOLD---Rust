//! Node descriptor — the human- and AI-facing metadata layer over the
//! runtime node registry.
//!
//! The runtime registry ([`PrimitiveFactory`](super::persistence::PrimitiveFactory))
//! carries only what the *executor* needs: a `type_id`, a constructor,
//! and optional editor-picker info. The *documentation* and *AI
//! composition* surfaces need more — a friendly one-line summary, a
//! creative category, a data-role, the precise technical `purpose`, and
//! discoverable examples. Rather than widen `PrimitiveFactory` (and
//! churn its ~18 construction sites + the load-bearing palette tests),
//! that metadata rides its own `inventory` channel: [`NodeDescriptor`].
//!
//! Two producers submit `NodeDescriptor`s:
//!
//! 1. The [`primitive!`](crate::primitive) macro emits one automatically
//!    for every macro-authored node — `purpose` is sourced from the
//!    existing `PrimitiveSpec::PURPOSE`, and the optional `summary` /
//!    `category` / `role` macro fields default to "unset".
//! 2. The ~15 hand-written `EffectNode` nodes (drivers, `wgsl_compute`,
//!    `mux_texture`, …) — which have no `PrimitiveSpec` — submit one by
//!    hand next to their `PrimitiveFactory`. This is where their
//!    `purpose` finally lives *in code* rather than only in the
//!    hand-maintained `docs/NODE_CATALOG.md`.
//!
//! [`catalog_gen`](super::catalog_gen) joins this channel with the
//! `PrimitiveFactory` registry (for `type_id` + picker label) and a
//! freshly-`create()`d instance (for live port/param shapes) to
//! generate `docs/NODE_CATALOG.md`'s node index and the machine-readable
//! `docs/node_catalog.json` the AI composition surface consumes.
//!
//! Fields are all `&'static` / `Copy`, so a `NodeDescriptor` is a const
//! literal — submittable straight into the `inventory` static channel.

/// Creative taxonomy: *what kind of job* a node does, in user-facing
/// terms. This is the axis a VJ thinks in ("I want to distort", "I want
/// noise") and the axis an AI searches by intent. Distinct from the
/// coarse [`PaletteCategory`](super::palette::PaletteCategory) (Atom /
/// Driver), which is the editor's two-strata layout, not a creative
/// grouping.
///
/// [`Uncategorized`](Self::Uncategorized) is the default for nodes that
/// haven't been classified yet — the naming pass fills the rest. The
/// starter taxonomy is intentionally small and provisional; expect it to
/// grow / be renamed once we sit with the full node list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Not yet classified. Default until the naming pass assigns one.
    Uncategorized,
    /// Brightness, saturation, hue, levels, tone mapping. Remaps colour
    /// without moving pixels.
    ColorAndTone,
    /// Spatial blur, sharpen, convolution kernels.
    BlurAndSharpen,
    /// Moves pixels in space. Warps, mirrors, kaleidoscope, RGB split.
    DistortAndWarp,
    /// Film looks and texture overlays (grain, dither, vignette, flash).
    Stylize,
    /// Produces an image from nothing. Shapes, patterns, gradients, text.
    Generate,
    /// Procedural noise sources (perlin, simplex, fbm, hash, voronoi).
    Noise,
    /// Produces a mask or falloff used to gate other effects.
    Mask,
    /// Combines two or more inputs into one (mix, wet/dry, multi blend).
    Composite,
    /// Meshes, projection, rotation, polytopes. The 3D and 4D pipeline.
    Geometry3D,
    /// Surface materials, lighting and shading atoms.
    MaterialsAndLighting,
    /// 2D particle and instance simulation.
    Particles2D,
    /// 3D particle and instance simulation.
    Particles3D,
    /// Control-rate sources and shapers: LFO, math, envelopes, triggers,
    /// beat-synced gates.
    Control,
    /// Reads information out of the image or the real world: depth, person,
    /// blobs, optical flow, luminance, peak, colour sampling.
    DetectionAndSampling,
    /// Numeric and structural plumbing (math ops, pack/unpack, scalar
    /// rescale, accumulators).
    MathAndConvert,
    /// Selects or routes signals (switch, downsample, wgsl compute).
    Routing,
    /// Coordinate and displacement maps with no look of their own. These
    /// carry a description rather than a friendly name.
    FieldsAndCoordinates,
}

impl Category {
    /// Stable, human-facing label. Used as the section heading in the
    /// generated catalog and the `category` string in the JSON artifact.
    pub fn label(self) -> &'static str {
        match self {
            Self::Uncategorized => "Uncategorized",
            Self::ColorAndTone => "Color & Tone",
            Self::BlurAndSharpen => "Blur & Sharpen",
            Self::DistortAndWarp => "Distort & Warp",
            Self::Stylize => "Stylize",
            Self::Generate => "Generate",
            Self::Noise => "Noise",
            Self::Mask => "Mask",
            Self::Composite => "Composite",
            Self::Geometry3D => "3D Geometry",
            Self::MaterialsAndLighting => "Materials & Lighting",
            Self::Particles2D => "Particles 2D",
            Self::Particles3D => "Particles 3D",
            Self::Control => "Control",
            Self::DetectionAndSampling => "Detection & Sampling",
            Self::MathAndConvert => "Math & Convert",
            Self::Routing => "Routing",
            Self::FieldsAndCoordinates => "Fields & Coordinates",
        }
    }
}

/// Data-role / *kind* of thing a node is in a graph — orthogonal to
/// [`Category`]. This is the TouchDesigner-suffix idea: it tells a human
/// (and an AI) how a node *wires*, which is exactly what a label can't
/// carry for the plumbing nodes (a `radial_offset_field` has no visible
/// effect — its role is `Map`, "produces a displacement consumed
/// downstream").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Not yet classified.
    Unknown,
    /// Produces an output from no image input — generators, noise, shapes.
    Source,
    /// Produces an intermediate field (coordinates / displacement /
    /// mask) meant to be *consumed* by another node, with no standalone
    /// visible result.
    Map,
    /// Image-in → image-out transform (the common effect atom).
    Filter,
    /// Terminal / output / render-to-target.
    Sink,
    /// Control-domain: emits a scalar / value that drives a parameter
    /// (the Driver stratum — LFO, envelope, math).
    Control,
}

impl Role {
    /// Stable, human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Source => "Source",
            Self::Map => "Map",
            Self::Filter => "Filter",
            Self::Sink => "Sink",
            Self::Control => "Control",
        }
    }
}

/// Documentation / AI-composition metadata for one node type, collected
/// from the whole binary via the `inventory` channel. See the module
/// docs for the two producers (macro + hand-written).
///
/// All fields are `&'static` / `Copy` so this is a const literal.
pub struct NodeDescriptor {
    /// Stable `type_id` — the join key against `PrimitiveFactory`.
    pub type_id: &'static str,
    /// Precise technical description (the existing `PrimitiveSpec::PURPOSE`
    /// for macro nodes). Reader-who-hasn't-seen-the-source level detail.
    pub purpose: &'static str,
    /// Friendly one-liner — what you'd say to a VJ. `""` until filled.
    pub summary: &'static str,
    /// Creative taxonomy. [`Category::Uncategorized`] until filled.
    pub category: Category,
    /// Data-role. [`Role::Unknown`] until filled.
    pub role: Role,
    /// Search synonyms: the node's old name, plain-English aliases, and the
    /// TouchDesigner-equivalent operator. Lets someone (or an AI) who knows
    /// another tool's vocabulary find the right node. Not shown on the node,
    /// only matched against in search. Empty until filled.
    pub aliases: &'static [&'static str],
    /// Names of preset graphs that use this node — discoverable examples.
    pub examples: &'static [&'static str],
}

inventory::collect!(NodeDescriptor);

/// Look up the descriptor for a `type_id`, if the node opted in.
/// Linear scan over the inventory channel — fine for the doc generator
/// (runs offline) and not on any hot path.
pub fn descriptor_for(type_id: &str) -> Option<&'static NodeDescriptor> {
    inventory::iter::<NodeDescriptor>
        .into_iter()
        .find(|d| d.type_id == type_id)
}

// ─── Hand-written nodes + boundary nodes ─────────────────────────────
//
// The `primitive!` macro auto-emits a `NodeDescriptor` for every
// macro-authored node (sourcing `purpose` from `PURPOSE`). The nodes
// below impl `EffectNode` by hand and have no `PrimitiveSpec`, so their
// `purpose` would otherwise live only in `docs/NODE_CATALOG.md` — the
// exact asymmetry that let docs drift. Registering them here moves that
// text into code where the drift guard covers it. `category` / `role`
// stay unset; the naming pass fills them (here for these, in the macro
// fields for the rest).
//
// Keeping the exception-set together (rather than scattering one submit
// into each of ~12 node files) is deliberate: the descriptor rides its
// own inventory channel, so co-location with the node buys nothing the
// `type_id` join doesn't already give, and one block is easier to audit.

macro_rules! hand_descriptor {
    // Minimal form: type_id + purpose, everything else unset.
    ($type_id:literal, $purpose:literal) => {
        hand_descriptor!(@build $type_id, $purpose, "", Category::Uncategorized, Role::Unknown, []);
    };
    // Full form: friendly summary, category, role, and search aliases.
    ($type_id:literal, $purpose:literal,
        summary: $summary:literal,
        category: $cat:ident,
        role: $role:ident,
        aliases: [ $($alias:literal),* $(,)? ] $(,)?
    ) => {
        hand_descriptor!(@build $type_id, $purpose, $summary, Category::$cat, Role::$role, [ $($alias),* ]);
    };
    // Internal builder.
    (@build $type_id:literal, $purpose:literal, $summary:expr, $cat:expr, $role:expr, [ $($alias:literal),* ]) => {
        inventory::submit! {
            NodeDescriptor {
                type_id: $type_id,
                purpose: $purpose,
                summary: $summary,
                category: $cat,
                role: $role,
                aliases: &[ $($alias),* ],
                examples: &[],
            }
        }
    };
}

// Color (color.rs)
hand_descriptor!(
    "node.brightness",
    "Pixel-local brightness multiply: out.rgb = in.rgb * brightness, alpha passes through.",
    summary: "Multiplies the image brightness up or down. A plain brightness control.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["brightness", "Level TOP"],
);
hand_descriptor!(
    "node.channel_mix",
    "4x4 RGBA matrix transform: each output channel is a weighted sum of the input RGBA plus a constant. Swizzle, desaturate, broadcast one channel to RGB, or apply any linear colour matrix.",
    summary: "Rebuilds each output channel as a mix of the input red, green, blue and alpha. Swizzle channels, build a custom black and white, or apply any colour matrix.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["channel mixer", "matrix", "swizzle", "Channel Mix TOP"],
);
hand_descriptor!(
    "node.color_ramp",
    "Map a scalar / luma input through a two-stop colour gradient (Color A to Color B). The palette-lookup atom behind tints and heat-map looks.",
    summary: "Remaps the image through a two-colour gradient based on brightness. Dark areas take the first colour, bright areas the second.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["gradient map", "color ramp", "duotone", "Lookup TOP"],
);

// Filter (filter.rs)
hand_descriptor!(
    "node.threshold",
    "Pixel-local luma threshold with a smoothstep falloff of width `softness`. Isolates bright regions for bloom and highlight masks.",
    summary: "Keeps only the bright parts of the image and drops the rest, with a soft edge you can widen. The way to pull out highlights for a bloom or a mask.",
    category: Mask,
    role: Filter,
    aliases: ["threshold", "highlight mask", "isolate", "key"],
);
hand_descriptor!(
    "node.blur",
    "Separable Gaussian blur, a horizontal then a vertical pass through a per-instance ping-pong texture. `radius` sets the kernel width.",
    summary: "Softens the image evenly in all directions, with a radius that sets how strong the blur is. The everyday blur.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["blur", "gaussian", "soft", "Blur TOP"],
);

// Routing (mux_texture.rs)
hand_descriptor!(
    "node.mux_texture",
    "Dynamic N-way Texture2D selector. `num_inputs` sets how many in_0..in_N ports exist and a rounded, clamped `selector` forwards the matching input. Reconfigures its port list when `num_inputs` changes.",
    summary: "Picks one of several incoming images and passes it through, chosen by a selector number. The input count grows as you wire more in.",
    category: Routing,
    role: Filter,
    aliases: ["switch", "mux", "selector", "Switch TOP"],
);

// WGSL escape hatch (wgsl_compute.rs)
hand_descriptor!(
    "node.wgsl_compute",
    "User-authored WGSL compute escape hatch. The shader is the contract: ports, uniform layout, workgroup size, binding map and output formats are all derived from the source via naga introspection. Backs effect families too varied to enumerate as fixed primitives.",
    summary: "A blank compute node you write your own WGSL shader into. The escape hatch for effects the built-in nodes don't cover, where the shader defines its own inputs and outputs.",
    category: Routing,
    role: Filter,
    aliases: ["wgsl", "compute", "custom shader", "GLSL TOP"],
);

// Control-rate scalar plumbing (driver primitives)
hand_descriptor!(
    "node.compressor_envelope",
    "Audio-compressor envelope path applied to a scalar signal level. Log-domain, program-dependent attack/release with ratio compression toward a target. Out is a gain multiplier in [0.1, 10.0]. Stateful.",
    summary: "Takes a signal level and produces a gain that ducks when the input is loud, the way an audio compressor rides the volume. Use it for auto-gain on brightness.",
    category: Control,
    role: Control,
    aliases: ["compressor", "auto gain", "envelope", "duck"],
);
hand_descriptor!(
    "node.envelope_decay",
    "Exponential one-shot decay. Snaps to 1.0 on each integer-edge change of `trigger`, then decays frame-rate-independently (env *= exp(-decay_rate · dt)). Drives clip-trigger envelope modes.",
    summary: "Snaps to full on each trigger then fades back to zero at a rate you set. The classic one-shot envelope for hits and flashes.",
    category: Control,
    role: Control,
    aliases: ["envelope decay", "decay", "one shot", "ar envelope"],
);
hand_descriptor!(
    "node.envelope_follower_ar",
    "Asymmetric attack/release envelope follower on a scalar. Switches time constant on rising (`attack`) vs falling (`release`) input. The audio-style counterpart to the symmetric node.smoothing.",
    summary: "Follows the level of a signal, rising fast on the attack and falling slow on the release, or however you set the two times. The asymmetric version of a smooth.",
    category: Control,
    role: Control,
    aliases: ["envelope follower", "attack release", "follower", "ar"],
);
hand_descriptor!(
    "node.inject_burst",
    "Fixed-duration burst state machine. On each new `trigger` (when enabled) runs a burst for `duration` seconds emitting active=1, a 0 to 1 phase ramp, and a stable hashed pick point. Drives FluidSim2D's inject mode.",
    summary: "On each trigger it runs a short timed burst, giving an active flag, a 0-to-1 ramp, and a random spot to inject at. Built for fluid sims that puff in new material on a hit.",
    category: Control,
    role: Control,
    aliases: ["inject burst", "burst", "puff", "impulse"],
);
hand_descriptor!(
    "node.sample_and_hold",
    "Capture an input scalar on each trigger-edge and hold it until the next edge. Freezes the trigger-time value so mid-decay slider moves don't leak through.",
    summary: "Grabs the value of a signal at each trigger and holds it steady until the next one. Freezes a moving value so later wiggles don't leak through.",
    category: Control,
    role: Control,
    aliases: ["sample and hold", "latch", "hold", "s&h"],
);
hand_descriptor!(
    "node.smoothing",
    "Exponential one-pole smoothing on a scalar wire. Response time is about `time_constant` seconds, frame-rate-independent. Symmetric (single time constant).",
    summary: "Smooths a jumpy control signal into a gentle glide, with the response time set in seconds. The same feel holds whatever the frame rate.",
    category: Control,
    role: Control,
    aliases: ["smoothing", "lag", "glide", "Lag CHOP"],
);
hand_descriptor!(
    "node.trigger_ease_to",
    "Beat-clocked snap-and-glide. On each trigger edge eases from the current value to the incoming `target` along a cubic ease-out over `window_beats` beats, then rests until the next trigger.",
    summary: "On each trigger it eases smoothly from its current value to a new target over a number of beats, then rests. A beat-clocked glide between values.",
    category: Control,
    role: Control,
    aliases: ["trigger ease", "glide", "ease to", "ramp to"],
);

// Vector field (rotate_vec2_by_angle.rs — legacy alias)
hand_descriptor!(
    "node.rotate_vec2_90",
    "Rotate the RG vec2 field by 90 degrees. Legacy type-ID alias of node.rotate_vec2_by_angle (which generalises to an arbitrary angle), retained so older presets load.",
    summary: "Rotates a 2D vector field by 90 degrees. The fixed-angle older version of Rotate Vector.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["rotate vector", "90 degrees", "turn"],
);

// Particle simulation (scatter_particles_camera.rs — legacy alias)
hand_descriptor!(
    "node.fluid_project_scatter_2d",
    "Legacy type-ID alias of node.scatter_particles_camera (FluidSim3D's camera-projection + 2D scatter display path), retained so older projects load.",
    summary: "Projects 3D particles through a camera and splats them to 2D. The older name for Draw Particles (camera).",
    category: Particles3D,
    role: Filter,
    aliases: ["draw particles camera", "project scatter"],
);

// Legacy fused wrappers (pending decomposition)
hand_descriptor!(
    "node.watercolor",
    "Pixel-exact wrap of the legacy WatercolorFX composite. Seven sequential passes: grain+max, flow, displacement, diffusion blur, slope displace, luma blur with persistent feedback, wet/dry. Legacy bundle pending decomposition.",
    summary: "A watercolor look built from a seven-pass feedback simulation, with grain, flow, diffusion, and soft bleeding edges. A legacy bundle still waiting to be decomposed.",
    category: Stylize,
    role: Filter,
    aliases: ["watercolor", "paint", "bleed", "wet"],
);
hand_descriptor!(
    "node.wireframe_depth",
    "Wraps the legacy WireframeDepthFX 15-pass pipeline (MiDaS depth DNN + optional optical flow + mesh pyramid) as a monolithic primitive, too tightly state-coupled to decompose yet. WireframeDepthGraph decomposition in flight.",
    summary: "A wireframe overlay driven by AI depth, drawing a mesh that follows the shape of whatever is in frame. A legacy bundle still being decomposed.",
    category: Stylize,
    role: Filter,
    aliases: ["wireframe depth", "depth wireframe", "mesh overlay"],
);

// Boundary nodes (boundary_nodes.rs)
hand_descriptor!(
    "system.source",
    "Effect-chain input boundary. The host pre-binds the upstream texture here.",
    summary: "The incoming image at the start of an effect chain. Wired in automatically.",
    category: Routing,
    role: Source,
    aliases: ["source", "input"],
);
hand_descriptor!(
    "system.generator_input",
    "Generator graph entry boundary. Emits the per-frame scalar context: time, beat, aspect, trigger_count, anim_progress.",
    summary: "The per-frame context a generator starts from, with time, beat, aspect, and trigger count. Wired in automatically.",
    category: Routing,
    role: Source,
    aliases: ["generator input", "context", "time beat"],
);
hand_descriptor!(
    "system.final_output",
    "Output boundary for both effect chains and generators. The host pre-binds the final output texture here.",
    summary: "The final image leaving a chain or generator. Wired in automatically.",
    category: Routing,
    role: Sink,
    aliases: ["output", "final output"],
);
