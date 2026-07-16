# Node Catalog

**Source of truth for what nodes exist.** Regenerate this file by walking [`crates/manifold-renderer/src/node_graph/primitives/`](../crates/manifold-renderer/src/node_graph/primitives/) (one `type_id` per primitive — `pub const *_TYPE_ID` for the composite-effect primitives, `type_id: "node.…"` for the macro-defined atoms) and the two preset directories ([`effect-presets/`](../crates/manifold-renderer/assets/effect-presets/), [`generator-presets/`](../crates/manifold-renderer/assets/generator-presets/)). If you add a primitive or a preset and don't update this catalog, the catalog is stale — fix it.

For *how* to compose these into a generator decomposition, see [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md). For the design rationale behind the primitive shape, see [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md).

---

## 1. Invariants

- **Type IDs are flat:** `node.<name>` for atoms and effects, `system.<name>` for boundary nodes. No category prefix — the category is presentation, not identity.
- **All primitives share one `Primitive` trait.** "Atom" vs "Effect" is how the palette groups them, not a structural split.
- **Port-shadows-param:** any scalar input port whose name matches a primitive `ParamDef` uses the wire when present and the param as the fallback. Standard for `gain`, `amount`, `rotation`, `wet_dry`, control-rate modulation everywhere.
- **Effects are JSON presets composed of atoms** (decomposed graphs in `effect-presets/`), drillable in the editor — not monolithic palette nodes. The fused effect-monolith bundles were deleted 2026-05-30; the lone surviving legacy `PostProcessEffect` wrapper is `node.wireframe_depth` (decomposition in flight). Preset names stay stable across implementation swaps so save files don't break.
- **Array port wires carry a Channels signature.** The macro syntax in this catalog uses `Array(T)` for typed families that have a `KnownItem` impl (`Particle`, `MeshVertex`, `EdgePair`, etc.) — equivalent to `Channels<T>`, both expand to an `ArrayType::of_known::<T>()` whose `specs` field carries the canonical channel list. For ad-hoc shapes, the inline form is `Channels[name: Type, ...]`. See [CHANNEL_TYPE_SYSTEM.md](CHANNEL_TYPE_SYSTEM.md) §4.1 for the type contract and §7 for the `well_known` channel-name registry that the canonical names resolve through.
- **`Array<T>` / `Channels<T>` outputs declare capacity** via `EffectNode::array_output_capacity`; the CI sweep test enforces this on the registry. Outputs also declare a non-empty Channels signature (either through `KnownItem::SPECS` or inline `Channels[...]`); the `every_conventional_array_port_declares_a_channels_signature` invariant gates that.
- **Stateful primitives declare** `state_lifecycle` + `state_capture_input_ports` so the StateStore knows where to break cycles. Per-port, not per-node.

### 1.1 Well-known channel-name registry (one-line overview)

Channels signatures reference `crate::node_graph::channel_names::well_known::*` constants for canonical names — `POSITION`, `VELOCITY`, `NORMAL`, `UV`, `WIDTH`, `HEIGHT`, `X`, `Y`, `Z`, `W`, `R`, `G`, `B`, `A`, `COLOR`, `A_INDEX`, `B_INDEX`, `LIFE`, `AGE`, `SEED`, `POS_SCALE`, `ROT`, `VALUE`, `T`, `INDEX`, `MAGNITUDE`, `PHASE`, `FREQ`, `CONFIDENCE`, `WEIGHT`. Adding a new canonical name: append one line inside the `well_known_channels!` macro invocation in [`crates/manifold-renderer/src/node_graph/channel_names.rs`](../crates/manifold-renderer/src/node_graph/channel_names.rs); the constant declaration and the collision-check test are generated from the same source list. Non-canonical names (one-off shapes, `wgsl_compute` shader field names) declare via inline string literals or naga's field-name walk and live in the runtime registry — they display correctly in editor tooltips, just aren't part of the canonical vocabulary.

---

## 2. Boundary nodes

| Display Name | Type ID | Inputs → Outputs | Purpose |
|---|---|---|---|
| Source | `system.source` | () → (Texture2D) | Effect-chain input — host pre-binds the upstream texture |
| Generator Input | `system.generator_input` | () → (time, beat, aspect, trigger_count, anim_progress) | Generator graph entry — scalar frame context |
| Final Output | `system.final_output` | (Texture2D) → () | Both surfaces — host pre-binds the chain / generator output texture |

---

## Registered node index (generated — authoritative)

This block is **generated from the node registry** by `gen_node_catalog` (`cargo run -p manifold-renderer --bin gen_node_catalog`) and is the drift-guarded source of truth for *what exists* — a registry change that isn't reflected here fails `cargo test`. The hand-curated "Atoms by intent" grouping below (§3) adds human structure and prose; once `category` / `role` are filled across the library, that grouping regenerates from those fields too. The full machine artifact — ports, params, complete descriptions, for the AI composition surface — is [`node_catalog.json`](node_catalog.json).

<!-- BEGIN GENERATED: registered-node-index — do not edit; run `cargo run -p manifold-renderer --bin gen_node_catalog` -->

_Generated from the node registry. Do not hand-edit. 242 nodes registered, grouped by category. Full ports, params, tooltips and search aliases live in [node_catalog.json](node_catalog.json)._

### Color & Tone (16)

| Node | type_id | role | summary |
|---|---|---|---|
| Brightness | `node.brightness` | Filter | Collapses colour to a single brightness value using per-channel weights — the default weighting matches how the eye perceives luminance. |
| Channel Mixer | `node.channel_mixer` | Filter | Remaps RGBA channels through a 4x4 matrix — swap, isolate, or blend channels into each other. |
| Clamp | `node.clamp` | Filter | Holds every colour between a low and high limit so nothing goes darker or brighter than you set. The tidy-up step after a math node. |
| Color LUT | `node.color_lut` | Filter | Remaps the image through a lookup-table strip indexed by brightness, the engine behind heat-map and infrared palettes. |
| Colorize | `node.colorize` | Filter | Tints the image toward a single colour, strongest on the bright neutral areas. Good for duotones and washes. |
| Contrast | `node.contrast` | Filter | Pushes the lights and darks apart for a punchier image, or pulls them together for a flatter one. It pivots around mid grey. |
| Dither | `node.dither` | Filter | Reduces the image to a few brightness levels and hides the banding with a fine noise pattern. The classic low-bit look. |
| Exposure | `node.exposure` | Filter | Brightens or darkens the whole image by multiplying every colour. Above 1 brightens, below 1 darkens, and 0 is black. |
| Gradient Map | `node.gradient_map` | Filter | Recolours an image by mapping its brightness onto a two-colour gradient — dark areas become one colour, bright areas another. |
| Hue / Saturation | `node.hue_saturation` | Filter | Spins the hue around the colour wheel and adjusts how vivid and bright the image is. The HSV way to recolour. |
| Invert | `node.invert` | Filter | Flips every colour to its opposite, turning a negative of the image. Blend it part-way for a partial invert. |
| Levels | `node.levels` | Filter | Reshapes brightness in one step with scale, offset, a clamp, and gamma. A compact way to lift shadows, crush highlights, or set black and white points. |
| Posterize | `node.posterize` | Filter | Crushes each colour into a small number of steps for a banded, blocky look. Fewer levels give a chunkier result. |
| Reinhard Tone Map | `node.reinhard_tone_map` | Filter | A simpler HDR-to-display tone map using the Reinhard curve. Lighter weight than the full Tone Map node. |
| Saturation | `node.saturation` | Filter | Pulls colours toward grey or pushes them more vivid. |
| Tone Map | `node.tone_map` | Filter | Fits HDR content, where colours can run far brighter than pure white, onto whatever display you are sending to. On a normal SDR screen or export it rolls the b… |

### Blur & Sharpen (9)

| Node | type_id | role | summary |
|---|---|---|---|
| Bilateral Blur | `node.bilateral_blur` | Filter | A depth-guided blur that smooths noise without bleeding across depth edges — the standard denoise pass after any per-pixel noisy sampler (ambient occlusion, di… |
| Blur | `node.blur` | Filter | Softens the image evenly in all directions, with a radius that sets how strong the blur is. The everyday blur. |
| Blur (3D) | `node.blur_3d` | Filter | Blurs a 3D volume one axis at a time, softening a density or flow field. Run it on each axis for an even blur in all directions. |
| Bokeh Gather | `node.bokeh_gather` | Filter | A true circular-aperture depth-of-field blur: each out-of-focus pixel gathers from a disc of neighbors sized by its own blur amount, and neighbors only contrib… |
| Custom Convolution | `node.custom_convolution` | Filter | Runs a custom 3x3 kernel over the image, so you can build your own blur, sharpen, edge-detect, or emboss from nine weights. For when the preset filters don't d… |
| Gaussian Blur | `node.gaussian_blur` | Filter | A single-axis Gaussian blur. Pair a horizontal pass with a vertical one for an even, soft blur in all directions. |
| Motion Blur | `node.motion_blur` | Filter | Smears each pixel along its own screen-space motion, scaled by the camera's shutter angle — the classic filmic motion-blur look, driven by real per-object move… |
| Sharpen | `node.sharpen` | Filter | Sharpens the image by boosting the difference between each pixel and its neighbours. At 0 it passes through, higher values make edges crisper. |
| Variable Blur | `node.variable_blur` | Filter | A Gaussian blur whose strength changes per pixel from a control image, so some areas blur more than others. Feed a mask or depth map into the width input for s… |

### Distort & Warp (8)

| Node | type_id | role | summary |
|---|---|---|---|
| Edge Stretch | `node.edge_stretch` | Map | Grabs a thin strip across the middle of the frame and smears it out to the edges, the classic slit-scan stretch. It outputs coordinates, so pair it with Remap. |
| Flip | `node.flip` | Filter | Mirrors the image across a line through the centre at any angle, so one half becomes a reflection of the other. Set the angle for a horizontal, vertical, or di… |
| Kaleidoscope | `node.kaleidoscope` | Map | Folds the image into a ring of mirrored wedges around a centre point. More segments give finer slices. It outputs warped coordinates, so pair it with Remap to … |
| Mirror | `node.mirror` | Map | Folds the image back on itself for mirror reflections, from a simple flip to a four-way quad mirror. It produces the folded coordinates, so feed it into Remap … |
| Radial Offset Field | `node.radial_offset_field` | Map | Makes a push outward from a centre point that other nodes use to shift pixels. It has no look of its own, so wire it into a displace or remap node. |
| Remap | `node.remap` | Filter | Resamples the image through a coordinate map, reading each pixel from wherever the map points. This is the node that turns a Mirror, Kaleidoscope, or any coord… |
| RGB Split | `node.rgb_split` | Filter | Pulls the red and blue channels apart along a direction you feed in, for a chromatic-aberration or glitchy colour-fringe look. The amount is in pixels and can … |
| Transform | `node.transform` | Filter | Moves, scales, and rotates the whole image around its centre. The basic transform for repositioning a layer. |

### Stylize (7)

| Node | type_id | role | summary |
|---|---|---|---|
| Dither Pattern | `node.dither_pattern` | Source | Generates the threshold grid that the Dither node uses to decide where pixels flip, with a choice of Bayer, halftone, and other patterns. Feed its output into … |
| Draw Scanlines | `node.draw_scanlines` | Filter | Adds faint monitor-style scanlines across the whole image. |
| Edge Detect | `node.edge_detect` | Filter | Finds the edges in the image and draws them as bright lines on dark, a Sobel outline. Crossfade it back over the source for a sketch look. |
| Film Grain | `node.film_grain` | Filter | Lays fine film-style grain over the image, heavier in the bright areas like real photographic stock. Dial the amount for a subtle texture or heavy noise. |
| Flash | `node.flash` | Filter | Pulses the whole image brighter, toward white, or toward black from a single amount. Wire a beat gate or envelope into the amount for strobes and hits. |
| Vignette | `node.vignette` | Filter | Darkens the edges of the frame to pull the eye inward, with a circle, oval, or rectangular falloff. The cinematic edge fade. |
| — | `node.watercolor` | Filter | A watercolor look built from a seven-pass feedback simulation, with grain, flow, diffusion, and soft bleeding edges. A legacy bundle still waiting to be decomp… |

### Generate (12)

| Node | type_id | role | summary |
|---|---|---|---|
| Basic Shape | `node.basic_shape` | Source | Draws one of three simple shapes, a square, diamond, or octagon, as a clean anti-aliased fill. Pick the shape, then size and rotate it. |
| Checkerboard | `node.checkerboard` | Source | Lays down an alternating black and white checker grid at any scale. Handy as a test pattern, a mask, or a base for tiled looks. |
| Draw Lines | `node.draw_lines` | Filter | Draws a set of smooth anti-aliased lines onto the image from a list of points. Used for wireframes, paths, and curve overlays. |
| Draw Rectangles | `node.draw_rectangles` | Filter | Draws a batch of filled rectangles onto the image from a list of positions and sizes. Good for bars, blocks, and data overlays. |
| glTF Texture | `node.gltf_texture_source` | Source | Loads an embedded image from a glTF/.glb file as a texture, so an imported model's baked-in albedo/alpha map flows into the render pipeline like any other text… |
| Gradient | `node.gradient` | Source | Builds a colour gradient as a strip you can use as a lookup table or feed into Gradient Map. Add as many colour stops as you like. |
| HDRI Source | `node.hdri_source` | Source | Loads a linear-HDR .exr environment map from disk as a texture, so a real-world HDRI capture flows into node.render_scene's envmap input like any other texture… |
| Image Folder | `node.image_folder` | Source | Plays through a folder of images with a single position knob, so you can scrub or sequence stills. Point it at a folder and drive the position. |
| Lightning Bolt | `node.lightning_bolt` | Source | Grows a jagged lightning bolt with branches each time it is struck — thick at the trunk, hairline at the tips. Feed its points and edges into Draw Lines. |
| Linear Gradient | `node.linear_gradient` | Source | A straight light-to-dark ramp across the frame at any angle. The simplest gradient, good for fades, masks, and ramps to drive other effects. |
| Render Text | `node.render_text` | Filter | Draws a text string onto the image with a chosen font, size, and position. Wire the text and font through the card so you can change them live. |
| Value Overlay | `node.value_overlay` | Filter | Prints small numeric labels onto the image at given spots using a built-in font. A quick readout for values flowing through a graph. |

### Noise (8)

| Node | type_id | role | summary |
|---|---|---|---|
| — | `node.fbm_2d` | Source | Octave-summed Perlin noise for rich fractal texture. The 4-octave Perlin branch of the unified Noise node. |
| Flow Field Noise | `node.flow_field_noise` | Source | Generates a swirling 2D flow field from layered noise, the velocity field you feed into advect or displace for fluid-like motion. |
| — | `node.hash_noise_field_2d` | Source | Uncorrelated per-pixel white noise, grain and dust and LIC ink. The Random branch of the unified Noise node. |
| Noise | `node.noise` | Source | Procedural noise in one node. Pick the Type, set the Scale, and raise Detail to stack octaves into rich fractal noise. Perlin, Simplex, and Value are smooth an… |
| — | `node.perlin_noise_2d` | Source | Classic Perlin gradient noise. The single-octave Perlin branch of the unified Noise node. |
| Simplex Field 2D | `node.simplex_field_2d` | Source | Signed simplex noise output as a field, used to drive flows and displacements rather than shown directly. |
| — | `node.simplex_noise_2d` | Source | Cleaner gradient noise with fewer directional artifacts than Perlin. The single-octave Simplex branch of the unified Noise node. |
| Voronoi 2D | `node.voronoi_2d` | Source | Cellular noise that gives each cell a distance and a stable random value. Good for tiles, foam, cracked glass and starfields. |

### Mask (7)

| Node | type_id | role | summary |
|---|---|---|---|
| Chroma Key | `node.chroma_key` | Filter | Outputs a mask showing how close each pixel is to a chosen colour, the green-screen key. Feed it into a mask mix to knock out a background. |
| Circle Mask | `node.circle_mask` | Source | Draws a soft-edged circle to limit an effect to a round region. It can stretch into an oval and rotate. |
| CoC Dilate | `node.coc_dilate` | Map | Spreads the maximum blur amount from a depth-of-field mask into its neighboring pixels, so the transition from sharp to blurry looks soft instead of having a h… |
| CoC From Depth | `node.coc_from_depth` | Map | Computes how out-of-focus each pixel should be from scene depth and a physical camera lens — the depth-of-field math, before any blurring happens. |
| Rectangle Mask | `node.rectangle_mask` | Source | Draws a soft-edged rectangle you can use to limit an effect to one region of the frame. Position it, size it, rotate it, and soften the edge. |
| SSAO (GTAO) | `node.ssao_gtao` | Map | Computes contact shadows from scene depth and a physical camera lens using a horizon-angle integral (GTAO) — darkens crevices and touching surfaces the way amb… |
| Threshold | `node.threshold` | Filter | Keeps only the bright parts of the image and drops the rest, with a soft edge you can widen. The way to pull out highlights for a bloom or a mask. |

### Composite (8)

| Node | type_id | role | summary |
|---|---|---|---|
| Feedback | `node.feedback` | Filter | Holds the previous frame and hands it back this frame, which lets you build feedback loops like trails and echoes. Wire its output back into the chain through … |
| HDR Mix | `node.hdr_mix` | Filter | Blends two images while keeping the bright above-white highlights from a reference, so a gain or grade doesn't crush the HDR detail. Reach for it when a proces… |
| Masked Mix | `node.masked_mix` | Filter | Blends two images using a third as a mask, applying one only where the mask is bright. The apply-only-where node. |
| Mix | `node.mix` | Filter | Blends two images together with a choice of modes like Add, Screen, Multiply, and Overlay, plus a crossfade amount. The core layer-blend node. |
| Multi Blend | `node.multi_blend` | Filter | Adds together any number of images and divides by a shared amount, collapsing a long chain of Mix(Add) nodes into one. Divisor 1 sums, divisor N averages. |
| Set Alpha | `node.set_alpha` | Filter | Forces the image's alpha to a fixed opacity while leaving the colours untouched. Ends a generator chain whose blends have eaten the alpha channel. |
| — | `node.texture_sum_5` | Filter | Legacy fixed five-input sum, superseded by node.multi_blend (dynamic N inputs). Hidden from the palette but still loads in saved graphs. |
| Wet/Dry | `node.wet_dry` | Filter | Crossfades a processed image back over the original, so you can dial how much of an effect shows. At 0 you get the original, at 1 the full effect. |

### 3D Geometry (44)

| Node | type_id | role | summary |
|---|---|---|---|
| Arrange Copies | `node.arrange_copies` | Source | Lays out a field of copies in a grid, ring, spiral, or random spread, giving each one a position to render at. Pair it with Render Copies. |
| Atmosphere | `node.atmosphere` | Source | Scene fog + sky tint for render_scene. Wire it into a scene's atmosphere input; put fog density on a fader for an instant depth-mood knob. |
| Bend Mesh | `node.bend_mesh` | Filter | Curves a mesh into an arc around a hinge line, like bending a rod. Position and lighting normals both rotate exactly, so it reads correctly at any angle, inclu… |
| Camera Lens | `node.camera_lens` | Filter | The physical camera: focus distance, aperture, shutter angle, and exposure — one lens any camera source can feed, and every 3D consumer reads. |
| Combine XY (curve) | `node.combine_xy` | Filter | Zips two number lists, X and Y, into one list of points ready to draw as a line or curve. |
| Cube Mesh | `node.cube_mesh` | Source | Builds a unit cube as a 3D mesh ready to rotate, light, and render. The starting block for box-based geometry. |
| Cylinder Wrap Field | `node.cylinder_wrap_field` | Map | Wraps a flat grid of points around a cylinder, placing copies on a curved surface. Part of the digital-plants geometry. |
| Digital Plants Render | `node.digital_plants_render` | Filter | Renders a field of cubes lit with shadows, the core of the Digital Plants look. A fused renderer still to be decomposed. |
| Edge Pairs | `node.edge_pairs` | Source | Connects a list of points in order into a single line, pairing each point with the next. Can close the loop back to the start. |
| Extrude Curve | `node.extrude_curve` | Source | Pushes a flat 2D shape straight through space to build a 3D extrusion — like a cookie cutter dragged through dough. Turns outlines into solid ribbons or bevele… |
| Facet Normals | `node.facet_normals` | Filter | Recomputes a mesh's normals from its own triangle geometry, giving flat, faceted shading — the exact fix for a mesh whose normals went stale after a heavy defo… |
| Flatten 3D → 2D | `node.flatten_3d` | Filter | Flattens a 3D mesh down to 2D points using a camera, so you can draw it as lines. The projection step for wireframe rendering. |
| Flatten 4D → 3D | `node.flatten_4d` | Filter | Flattens 4D geometry like a tesseract down toward 3D, the first step in drawing a four-dimensional shape. |
| Free Camera | `node.free_camera` | Source | A free-look camera positioned and aimed directly with Euler angles, instead of orbiting a target. Gizmo- and import-friendly. |
| glTF Mesh | `node.gltf_mesh_source` | Source | Loads a glTF/.glb model file from disk as mesh geometry, so imported 3D assets flow into the render pipeline like any other shape primitive. |
| Grid Edges | `node.grid_edges` | Source | Outputs the wireframe edges that connect a grid of points, so you can draw the grid as a mesh of lines. |
| Grid Mesh | `node.grid_mesh` | Source | Builds a flat grid of points as a 3D mesh, the base for terrain, cloth, and displacement looks. Pair it with Surface Bumps or Push Mesh. |
| Grid Points (UV) | `node.grid_points` | Source | Outputs a grid of U and V values sampling a parametric surface, the input for building curved meshes and wireframes. |
| Hypercube Edges (4D) | `node.hypercube_edges` | Source | Builds the wireframe edges of a hypercube — which corners connect. Feed it with the matching hypercube points to draw the 4D cube. |
| Hypercube Points (4D) | `node.hypercube_points` | Source | Builds the corner points of a hypercube. The Dimension knob morphs it from a flat square up through a cube to a full 4D tesseract — wire it to an LFO to animat… |
| Look-At Camera | `node.look_at_camera` | Source | A camera positioned directly and aimed at a target point, instead of orbiting or using Euler angles. |
| Make Triangles | `node.make_triangles` | Filter | Turns a grid of points into a solid mesh of triangles, so a flat field of points becomes a surface you can render. |
| Mesh Edges | `node.mesh_edges` | Filter | Outputs the wireframe edges of a triangle mesh, so any imported model can be drawn as lines. The mesh counterpart of Grid Edges. |
| Mesh Ramp | `node.mesh_ramp` | Source | Turns a mesh's own positions into a growth mask — a value from 0 to 1 per vertex that sweeps across the mesh along an axis. Feeds any deformer's weight input t… |
| Morph Mesh | `node.morph_mesh` | Filter | Blends smoothly between two meshes vertex-by-vertex, so one shape dissolves into another. Works best when both meshes share the same vertex count and layout. |
| Nested Cubes Geometry | `node.nested_cubes_geometry` | Source | Renders a field of nested, rotating cubes with per-face scatter and a beat-driven kick. A self-contained generator, still to be broken into atoms. |
| Orbit Camera | `node.orbit_camera` | Source | A camera that orbits around a target point, with controls for distance, height, and angle. The viewpoint for 3D mesh rendering. |
| Platonic Solid Edges | `node.platonic_solid_edges` | Source | Builds the wireframe edges of one of the five Platonic solids, pairing up which corners connect. Feed it with the matching points to draw the wireframe. |
| Platonic Solid Points | `node.platonic_solid_points` | Source | Builds the corner points of one of the five Platonic solids, from a tetrahedron to a dodecahedron. The vertex set for wireframe geometry. |
| Push Along Normals | `node.push_along_normals` | Filter | Pushes every point of a mesh outward or inward along its own surface direction — the 3D version of a bulge or breathe effect, optionally masked and driven by a… |
| Push Mesh | `node.push_mesh` | Filter | Pushes a mesh's points up and down by reading a height image, turning a flat grid into bumpy terrain. The 3D version of a displacement. |
| Render Copies | `node.render_copies` | Filter | Draws many copies of one mesh in a single pass, each placed by a list of transforms. The fast way to render a field of repeated objects. |
| Render Mesh | `node.render_mesh` | Filter | Draws a 3D mesh to the screen with a camera, a light, and a material. The final step that turns geometry into an image. |
| Render Scene | `node.render_scene` | Filter | Draws several 3D objects into one scene so the nearer ones correctly block the farther ones, each with its own position and material, lit by any number of shar… |
| Repeat Outline (rings) | `node.repeat_outline` | Filter | Stacks scaled copies of an outline into concentric rings, turning one shape into a set of nested rings. |
| Revolve Curve | `node.revolve_curve` | Source | Spins a 2D profile curve around a vertical axis to build a solid of revolution — a lathe. The classic way to build vases, columns, and bells from a cross-secti… |
| Rotate 3D | `node.rotate_3d` | Filter | Spins a 3D mesh around the X, Y, and Z axes. Wire an LFO or a beat into the angles to keep it turning. |
| Rotate 4D | `node.rotate_4d` | Filter | Spins 4D geometry through its rotation planes, the move that makes a tesseract appear to turn inside out. |
| Scatter On Mesh | `node.scatter_on_mesh` | Source | Scatters copies of an object across a mesh's surface — a field of instances placed and sized randomly but deterministically, area-weighted so they don't clump … |
| Taper Mesh | `node.taper_mesh` | Filter | Narrows a mesh toward a point along one axis, like sharpening a pencil or a candle flame. The lighting normals scale with it so the taper still shades correctl… |
| Torus Wrap Field | `node.torus_wrap_field` | Map | Wraps a flat grid of points around a torus, a donut shape, placing copies on its surface. |
| Transform 3D | `node.transform_3d` | Source | Position, rotation, and scale for one scene object. Wire it into a render_scene transform slot, or drive an axis from an LFO or MIDI to animate it live. |
| Tube From Path | `node.tube_from_path` | Source | Sweeps a tube of adjustable thickness along a path — the way you'd build a vine, cable, or ribbon from a center-line curve. Thickness and lift can vary per poi… |
| Twist Mesh | `node.twist_mesh` | Filter | Twists a mesh around its own length, like wringing out a cloth or spinning a vine. Position and lighting normals both rotate exactly, so continuous saw-LFO spi… |

### Materials & Lighting (11)

| Node | type_id | role | summary |
|---|---|---|---|
| Bake Environment (equirect) | `node.bake_environment` | Source | Builds a studio environment map for reflections, laid out as an equirectangular panorama. Feed it into a PBR material for image-based lighting. |
| Basic Light (Lambert) | `node.basic_light` | Filter | Shades a surface from its normal map and a single direction, brightest where it faces the light. The plain matte lighting term. |
| Cel Material | `node.cel_material` | Source | A toon material that snaps the lighting into a few flat bands for a cartoon or cel-shaded look. |
| Light | `node.light` | Source | A single light source for 3D scenes, set to a sun for parallel rays or a point for a local glow. Wire it into a material or a mesh renderer. |
| Matcap Two-Tone | `node.matcap_two_tone` | Filter | Shades a surface by mapping its normals into a two-tone sphere lookup, a fast stylised material that needs no real lights. |
| PBR Material | `node.pbr_material` | Source | A physically based material with roughness, metalness, and environment reflections. The realistic workhorse for 3D surfaces. |
| Phong Material | `node.phong_material` | Source | A basic shiny material with soft diffuse shading and a sharp highlight. The cheap go-to for lit 3D surfaces. |
| Rim Light (Fresnel) | `node.rim_light` | Filter | Lights up the edges of a surface where it turns away from the camera, the glowing rim you see on backlit objects. |
| Shininess (Blinn) | `node.shininess` | Filter | Adds a tight highlight where the surface catches the light, set by a shininess amount. The glossy hotspot on top of basic lighting. |
| Surface Bumps | `node.surface_bumps` | Filter | Turns a grayscale height image into a normal map, so light and dark become bumps and dents the lighting can catch. The way to add surface detail from a texture. |
| Unlit Material | `node.unlit_material` | Source | A flat-colour material with no lighting, so the surface shows its base colour straight. The simplest material, good for solid or glowing looks. |

### Particles 2D (16)

| Node | type_id | role | summary |
|---|---|---|---|
| Add Burst (radial) | `node.add_burst` | Filter | Pushes particles outward from a point in a burst, like an explosion or shockwave on a hit. |
| Anti-Clump Particles | `node.anti_clump_particles` | Filter | Nudges particles apart where they bunch up, keeping the cloud evenly spread instead of collapsing into blobs. |
| Blend Copies | `node.blend_copies` | Filter | Blends two arrangements of copies together by an amount, so you can morph a field of copies from one layout to another. |
| Draw Particles (scatter) | `node.draw_particles` | Filter | Splats a cloud of particles onto a buffer, building up an image from where they land. Pair it with Resolve Scatter to read the result back. |
| Explosion Force | `node.explosion_force` | Source | Makes a force field that pushes outward from a point, the field you feed into a particle move to drive an explosion. |
| Fractal Noise (per copy) | `node.fractal_noise_per_copy` | Filter | Gives every copy its own fractal-noise value, a smooth random number per copy you can drive size, colour, or motion with. |
| Move Particles (Euler step) | `node.move_particles` | Filter | Moves every particle one step along its velocity each frame. The basic integrator that makes a particle system actually move. |
| Position Jitter | `node.position_jitter` | Filter | Adds a random offset to each copy's position with noise, so a perfect grid of copies looks more natural and scattered. |
| Rotation Jitter | `node.rotation_jitter` | Filter | Adds a random twist to each copy's rotation, so a field of copies face slightly different ways instead of lining up. |
| Sample Image for Particles | `node.sample_image_at_particles` | Filter | Reads the image colour underneath each particle, so the particles can pick up the look of whatever they fly over. |
| Simplex Noise (per copy) | `node.simplex_noise_per_copy` | Filter | Gives every copy its own simplex-noise value, a smooth random number per copy for varying the look across a field. |
| Spawn From Image | `node.spawn_from_image` | Source | Creates particles placed by the bright areas of an image, so a picture or mask becomes a cloud of points. Spawn density follows the image. |
| Spawn Particles | `node.spawn_particles` | Source | Creates a fresh batch of particles to start a simulation, with a count you set. The first node in a particle chain. |
| Spread Out (diffuse) | `node.spread_out` | Filter | Gives each particle a small random kick so a tight clump slowly spreads apart. Adds a bit of life and scatter. |
| Turbulence (simplex) | `node.turbulence` | Filter | Pushes particles around with a flowing noise field, giving organic, swirling motion. The classic turbulence force. |
| Wrap Around (torus) | `node.wrap_around` | Filter | Wraps particles back to the opposite edge when they leave the frame, so the cloud loops seamlessly instead of escaping. |

### Particles 3D (13)

| Node | type_id | role | summary |
|---|---|---|---|
| Add Burst (3D, radial) | `node.add_burst_3d` | Filter | Injects 3D particles in a burst around one of a few fixed zones, puffing new material into a 3D sim on a hit. |
| Draw Particles (3D scatter) | `node.draw_particles_3d` | Filter | Splats 3D particles into a volume buffer, building up a 3D density field from where they land. The 3D version of Draw Particles. |
| Draw Particles (camera) | `node.draw_particles_camera` | Filter | Projects 3D particles through a camera and splats them onto a 2D image in one step. The display path for a 3D particle sim. |
| Flatten to Camera Plane | `node.flatten_to_camera_plane` | Filter | Squashes a cloud of 3D particles flat toward the camera by a dial-able amount, from a full volume down to a pancake facing the screen. |
| Keep In Box (3D) | `node.keep_in_box_3d` | Filter | Holds 3D particles inside their container, either wrapping them around or bouncing them back at the edges. The hard boundary after a move. |
| Move Particles (3D, Euler step) | `node.move_particles_3d` | Filter | Moves every 3D particle one step along its velocity each frame. The integrator for a 3D particle system. |
| Push From Walls (3D) | `node.push_from_walls_3d` | Filter | Pushes 3D particles gently away from the walls of their container as they get close, keeping them inside without a hard bounce. |
| Remove Drift (3D) | `node.remove_drift_3d` | Filter | Balances the forces on a particle system so it stops slowly sliding in one direction — a long-running fluid stays centered instead of silting into a corner. |
| Sample Volume for Particles (3D) | `node.sample_volume_at_particles` | Filter | Reads a 3D volume at each particle's position, so particles can pick up a value from a density or flow field they pass through. |
| Spawn From Mesh | `node.spawn_from_mesh` | Source | Creates particles from a mesh's own geometry — one per vertex for an exact silhouette, or scattered evenly across its surface. The way an imported model dissol… |
| Spread Out (3D diffuse) | `node.spread_out_3d` | Filter | Gives each 3D particle a small random kick so a tight clump slowly spreads apart in space. |
| Swirl Force (3D, curl) | `node.swirl_force_3d` | Filter | Turns a 3D gradient field into a swirling, divergence-free force, the move that makes 3D particles curl into smoke-like eddies. |
| Turbulence (3D, simplex) | `node.turbulence_3d` | Filter | Pushes 3D particles around with a flowing 3D noise field for organic, swirling motion through space. |

### Control (21)

| Node | type_id | role | summary |
|---|---|---|---|
| Beat Gate | `node.beat_gate` | Control | A square pulse locked to the tempo, on for part of each beat and off for the rest. The strobe and chop building block. |
| Beat Ramp | `node.beat_ramp` | Control | Rises from 0 to 1 across each beat then snaps back, a sawtooth locked to the tempo. Wire it into anything you want to sweep in time with the music. |
| Canvas Area Scale | `node.canvas_area_scale` | Control | Outputs how big the canvas is compared to a reference size, used to keep particle brightness steady when the resolution changes. |
| Clip Trigger Cycle | `node.clip_trigger_cycle` | Control | Steps through a range on each clip trigger, never landing on the same value twice in a row. Drives never-repeat preset cycling. |
| Clip Trigger Index | `node.clip_trigger_index` | Control | Counts how many times a clip has been triggered and wraps it to a range, so each retrigger steps to the next slot. Drives preset cycling. |
| Compressor Envelope | `node.compressor_envelope` | Control | Takes a signal level and produces a gain that ducks when the input is loud, the way an audio compressor rides the volume. Use it for auto-gain on brightness. |
| Cycle Table Row | `node.cycle_table_row` | Control | Steps through the rows of a small built-in table on each clip trigger, emitting one row of numbers at a time. A way to sequence preset values. |
| Envelope Decay | `node.envelope_decay` | Control | Snaps to full on each trigger then fades back to zero at a rate you set. The classic one-shot envelope for hits and flashes. |
| Envelope Follower (A/R) | `node.envelope_follower_ar` | Control | Follows the level of a signal, rising fast on the attack and falling slow on the release, or however you set the two times. The asymmetric version of a smooth. |
| Frequency Ratio | `node.frequency_ratio` | Control | Emits a pair of small whole-number ratios from a musical-interval table. Use it for Lissajous curves and similar shapes where the X and Y rates set the form. |
| glTF Animation Source | `node.gltf_animation_source` | Source | Plays back an imported glTF animation clip. Wire its outputs into a Transform 3D node to animate an imported object, or leave the progress input unwired to loo… |
| Inject Burst | `node.inject_burst` | Control | On each trigger it runs a short timed burst, giving an active flag, a 0-to-1 ramp, and a random spot to inject at. Built for fluid sims that puff in new materi… |
| LFO | `node.lfo` | Control | A smoothly cycling value you wire into any knob to make it move on its own. Pick a waveform like sine or saw, and lock it to the tempo or let it run free. |
| Math | `node.math` | Control | Combines two control signals into one with a chosen op, like add, multiply, min, or max. The basic calculator for modulation. |
| One Euro Filter | `node.one_euro_filter` | Control | Smooths a jittery signal but lets fast moves through cleanly, so it removes noise without the laggy feel of a plain smooth. Great for hand-tracked or sensor in… |
| Sample & Hold | `node.sample_and_hold` | Control | Grabs the value of a signal at each trigger and holds it steady until the next one. Freezes a moving value so later wiggles don't leak through. |
| Scale + Offset (value) | `node.scale_offset_value` | Control | Multiplies a value by a scale and adds an offset, the everyday way to rescale a control signal into the range a knob wants. Set the scale negative to invert. |
| Smoothing | `node.smoothing` | Control | Smooths a jumpy control signal into a gentle glide, with the response time set in seconds. The same feel holds whatever the frame rate. |
| Trigger Ease To | `node.trigger_ease_to` | Control | On each trigger it eases smoothly from its current value to a new target over a number of beats, then rests. A beat-clocked glide between values. |
| Trigger Gate | `node.trigger_gate` | Control | Passes a trigger stream through only while it is enabled, so you can switch a clip-trigger source on and off. |
| Value | `node.value` | Source | Outputs a single fixed number you set by hand. Wire it into any knob as a constant, or expose it to drive from outside. |

### Detection & Sampling (15)

| Node | type_id | role | summary |
|---|---|---|---|
| Filter Detections | `node.array_filter_detections` | Filter | Drops junk detections that are too small, too stretched, or cover too much of the frame, before they reach the tracker. Stops a HUD from locking onto the horiz… |
| Blob Overlay | `node.blob_overlay` | Filter | Draws boxes around each tracked blob on top of the image, so you can see what the Blob Tracker is finding. A debug view for blob tracking. |
| Blob Tracker | `node.blob_tracker` | Filter | Finds bright blobs in the image and tracks them frame to frame, handing back their positions and sizes as a list. The base for blob-reactive visuals. |
| Color Sample | `node.color_sample` | Control | Reads the colour at a single point in the image and outputs its RGB and brightness. An eyedropper you can drive an effect from. |
| Depth Map | `node.depth_map` | Filter | Estimates a depth map from any flat image with an AI model, so nearer things read bright and far things dark. Feed it into a blur or displace to fake 3D from 2… |
| Draw Connections | `node.draw_connections` | Filter | Draws dashed lines linking tracked objects that are near each other, with an optional dot at the middle of each link. |
| Draw Dots | `node.draw_dots` | Filter | Draws a small glowing dot at the centre of every tracked object. |
| Draw Gauge | `node.draw_gauge` | Filter | Draws a small readout bar under every tracked object that fills up as the object gets bigger. |
| Draw Markers | `node.draw_markers` | Filter | Draws a marker on every tracked object: corner brackets around it or a crosshair at its centre. The building block for tracking overlays. |
| Draw Ticks | `node.draw_ticks` | Filter | Draws small measurement-style tick marks beside every tracked object. |
| Luminance | `node.luminance` | Control | Measures the average brightness of the image and outputs it as a single number. Wire it into a knob to make an effect react to how bright the picture is. |
| Optical Flow | `node.optical_flow` | Filter | Measures how the image is moving between frames and outputs that motion as a flow field. Drive a displace or advect with it to push pixels along the motion. |
| Peak | `node.peak` | Control | Measures the brightest point in the image and outputs it as a single number. Reacts to the highlights rather than the overall brightness. |
| Person Mask | `node.person_mask` | Filter | Finds people in the image with an AI model and outputs a mask that is white on the person and black elsewhere. Use it to cut someone out or key effects to them. |
| Track Persist | `node.track_persist` | Filter | Keeps a stable identity on each tracked blob from frame to frame, holding onto one briefly even if it flickers out. Stops IDs from jumping around. |

### Math & Convert (19)

| Node | type_id | role | summary |
|---|---|---|---|
| Absolute Value | `node.absolute_value` | Filter | Flips every negative value positive, leaving positives alone. Handy after a signed field or a sine to fold it into a V shape. |
| Array Feedback | `node.array_feedback` | Filter | Holds a list from the previous frame and hands it back this frame, closing a feedback loop for a particle or instance system without a graph cycle. |
| Array Math | `node.array_math` | Filter | Runs the same math over every number in a list, like add, multiply, sine, or scale. The list-wide version of the Math node. |
| Combine XYZW | `node.combine_xyzw` | Filter | Zips four separate number lists into one list of 4D points. The 4D counterpart to combining X and Y into a curve. |
| Connect Nearest | `node.connect_nearest` | Control | For each item in a list, finds its nearest neighbour and emits a connecting line. Used to draw constellations between tracked blobs. |
| Normalize | `node.normalize` | Filter | Scales the red and green channels read as a 2D vector down to length 1, keeping the direction and dropping the magnitude. |
| Pack RGBA | `node.pack_rgba` | Filter | Combines four single-channel images into one RGBA image, one image per colour channel. The opposite of pulling an image apart. |
| Power | `node.power` | Filter | Raises each value to a power, which sharpens or softens a 0-to-1 field. Above 1 pushes toward black, below 1 lifts the midtones. |
| Range | `node.range` | Source | Builds a list of evenly spaced numbers between a start and an end. The starting point for laying out copies, rings, or steps. |
| Resolve Scatter | `node.resolve_scatter` | Filter | Reads back the buffer that Draw Particles wrote into and turns it into a normal image. The pickup step after a particle splat. |
| Resolve Scatter (3D) | `node.resolve_scatter_3d` | Filter | Reads back the 3D buffer that a 3D particle scatter wrote into and turns it into a volume you can sample. |
| Scale + Offset (image) | `node.scale_offset_image` | Filter | Multiplies each colour by a scale and adds an offset, the image version of a basic value remap. Re-range a field before a clamp or a math step. |
| Sine / Cosine | `node.sine_cosine` | Filter | Runs each value through sine, cosine, or tangent after scaling it. The building block for ripples and wave patterns out of a gradient. |
| Smoothstep | `node.smoothstep` | Filter | Eases each value through a smooth S-curve between a low and high edge. Softens a hard threshold into a gentle ramp. |
| Split XY | `node.split_xy` | Filter | Splits a list of 2D points into two separate number lists, one for X and one for Y. The inverse of combining them. |
| Sum Into Bins | `node.sum_into_bins` | Control | Adds an amount into each slot of a running list on every trigger, so you can build up a histogram or per-slot counter over time. |
| Texture Size | `node.texture_size` | Control | Reads the width, height, and aspect ratio of an image and hands them back as numbers. Wire the aspect into a mask to keep circles round on a wide canvas. |
| Vector Length | `node.vector_length` | Filter | Measures the length of the red and green channels read as a 2D vector, giving the strength of a flow or gradient field. |
| Wrap | `node.wrap` | Filter | Keeps only the part after the decimal point, which wraps every value back into 0 to 1. Multiply the input first to tile or repeat a gradient. |

### Routing (8)

| Node | type_id | role | summary |
|---|---|---|---|
| Downsample | `node.downsample` | Filter | Shrinks the image by a whole-number factor with a box filter, trading detail for speed. Good before a heavy effect or for a blocky look. |
| Switch (array) | `node.switch_array` | Filter | Picks one of several incoming lists and passes it through, chosen by a selector number. |
| Switch (texture) | `node.switch_texture` | Filter | Picks one of several incoming images and passes it through, chosen by a selector number. The input count grows as you wire more in. |
| Switch (value) | `node.switch_value` | Filter | Picks one of several incoming values and passes it through, chosen by a selector number. Use it to flip between sources live. |
| WGSL Compute | `node.wgsl_compute` | Filter | A blank compute node you write your own WGSL shader into. The escape hatch for effects the built-in nodes don't cover, where the shader defines its own inputs … |
| — | `system.final_output` | Sink | The final image leaving a chain or generator. Wired in automatically. |
| — | `system.generator_input` | Source | The per-frame context a generator starts from, with time, beat, aspect, and trigger count. Wired in automatically. |
| — | `system.source` | Source | The incoming image at the start of an effect chain. Wired in automatically. |

### Fields & Coordinates (20)

| Node | type_id | role | summary |
|---|---|---|---|
| Block Displace Field | `node.block_displace_field` | Source | Outputs a grid of random block offsets, the displacement map behind datamosh and block-glitch looks. Feed it into Remap. |
| Centered UV | `node.centered_uv` | Source | Outputs each pixel's position measured from a centre point, so the middle reads zero and the edges spread out. The base for radial and zoom effects. |
| Distance to Point | `node.distance_to_point` | Source | Outputs how far each pixel is from a chosen point, bright far away and dark near it. A radial gradient you build circle masks and ripples from. |
| Edge Slope | `node.edge_slope` | Filter | Measures how fast a value changes across the image, giving the direction and steepness of edges. The base for normal maps and edge effects. |
| Edge Slope (3D) | `node.edge_slope_3d` | Filter | Measures how fast a value changes through a 3D volume, giving a direction at every point. Used to find flow and forces inside a fluid sim. |
| Field Combine | `node.field_combine` | Map | Mixes the channels of a coordinate field into one value with weights and an offset. The math step that turns coordinates into a custom gradient. |
| Flow Lines (LIC) | `node.flow_lines` | Filter | Smears noise along a flow field to reveal its streamlines, turning a vector field into a visible flow texture. |
| Grid UV Field | `node.grid_uv_field` | Source | Outputs a grid of sample points across the frame as a list, used to drive instanced shapes or sample a field at regular spots. |
| Hash Field by Seed | `node.hash_field_by_seed` | Map | Scrambles a coordinate field by a seed so the same input gives a different but stable random offset per seed. Used to re-randomise a pattern on a trigger. |
| Smooth (neighbors) | `node.neighbor_smooth` | Filter | Averages each point with its neighbours on a grid, smoothing out a bumpy field of values or positions. |
| Polar Field | `node.polar_field` | Source | Outputs each pixel's angle and distance from a centre instead of its X and Y. The base for spirals, tunnels, and kaleidoscopes. |
| Rotate Coordinates | `node.rotate_coordinates` | Map | Rotates a coordinate field around the centre. This spins the coordinates used to build a warp, not the image itself. For the picture, use Flip or a transform. |
| Rotate Vector | `node.rotate_vector` | Map | Rotates a 2D vector field by an angle, turning every arrow in a flow or gradient field by the same amount. |
| Scanline Jitter Field | `node.scanline_jitter_field` | Source | Per-row horizontal offset for sideways glitch. Tear = gated VHS jolt; Slide = smooth organic per-band drift. Set Bands for chunky strips, feed it into Remap. |
| Sine Wave (projected) | `node.sine_wave` | Map | Mixes a coordinate field into a moving sine wave in one step, the core ingredient of plasma and interference patterns. |
| Slice Volume | `node.slice_volume` | Filter | Takes a flat slice through a 3D volume to get a normal 2D image. The way to look inside a fluid or density field. |
| Slope Displace | `node.slope_displace` | Filter | Pushes pixels along the slope of an embossed version of the image, an emboss-driven warp for liquid and paint looks. |
| Texture Advect | `node.texture_advect` | Filter | Drags a texture along a velocity field, carrying the pixels with the flow. The transport step in a fluid simulation. |
| UV Displace by Flow | `node.uv_displace_by_flow` | Filter | Samples the image at positions pushed by a flow field, so the picture smears along the motion. The consumer for an optical-flow or noise flow field. |
| UV Field | `node.uv_field` | Source | Outputs the position of each pixel as a coordinate, red for left-to-right and green for top-to-bottom. The starting grid for most warps and patterns. |

### Effect & generator presets (51)

| id | name | kind | category | params |
|---|---|---|---|---|
| `ApricotWeather` | Apricot Weather | generator | Geometry | 5 |
| `AutoGain` | Auto Gain | effect | Stylize | 4 |
| `BasicShapes` | Basic Shapes | generator | Pattern | 4 |
| `BlackHole` | Black Hole | generator | Sim | 18 |
| `BlobTracking` | Blob Track | effect | Diagnostic | 5 |
| `Bloom` | Bloom | effect | Filmic | 1 |
| `BlossomWire` | Blossom Wire | generator | Geometry | 11 |
| `Caustics` | Caustics | generator | Pattern | 4 |
| `ChromaticAberration` | Chromatic Aberration | effect | Filmic | 5 |
| `ColorCompass` | Color Compass | effect | Spatial | 2 |
| `ColorGrade` | Color Grade | effect | Color | 9 |
| `ConcentricTunnel` | Concentric Tunnel | generator | Pattern | 6 |
| `Cymatics` | Cymatics | generator | Pattern | 7 |
| `DepthOfField` | Depth of Field | effect | Filmic | 8 |
| `DigitalDrift` | Digital Drift | effect | Filmic | 4 |
| `DigitalPlants` | Digital Plants | generator | Geometry | 14 |
| `Dither` | Dither | effect | Color | 2 |
| `Duocylinder` | Duocylinder | generator | Geometry | 11 |
| `EdgeDetect` | Edge Detect | effect | Diagnostic | 2 |
| `EdgeStretch` | Edge Stretch | effect | Spatial | 3 |
| `FilmGrain` | Film Grain | effect | Stylize | 2 |
| `FluidSim2D` | Fluid Sim 2D | generator | Sim | 13 |
| `FluidSim3D` | Fluid Sim 3D | generator | Sim | 22 |
| `Glitch` | Glitch | effect | Filmic | 5 |
| `HighlightBoost` | Highlight Boost | effect | Filmic | 4 |
| `Infrared` | Infrared | effect | Color | 3 |
| `Invert` | Invert | effect | Color | 1 |
| `Kaleidoscope` | Kaleidoscope | effect | Spatial | 2 |
| `Lightning` | Lightning | generator | Pattern | 7 |
| `Lissajous` | Lissajous | generator | Geometry | 11 |
| `MetallicGlass` | Metallic Glass | generator | Sim | 13 |
| `Mirror` | Mirror | effect | Spatial | 2 |
| `MriVolume` | MRI Volume | generator | Text & Media | 8 |
| `NestedCubes` | Nested Cubes | generator | Geometry | 6 |
| `NodeGraphTest` | Node Graph Test | effect | Diagnostic | 1 |
| `OilyFluid` | Oily Fluid | generator | Sim | 14 |
| `ParticleText` | Particle Text | generator | Text & Media | 15 |
| `Plasma` | Plasma | generator | Pattern | 6 |
| `QuadMirror` | Quad Mirror | effect | Spatial | 1 |
| `SoftFocus` | Soft Focus | effect | Stylize | 2 |
| `StarField` | Star Field | generator | Pattern | 8 |
| `StrangeAttractor` | Strange Attractor | generator | Sim | 11 |
| `Strobe` | Strobe | effect | Stylize | 4 |
| `StylizedFeedback` | Stylized Feedback | effect | Stylize | 3 |
| `Tesseract` | Tesseract | generator | Geometry | 12 |
| `Text` | Text | generator | Text & Media | 9 |
| `Transform` | Transform | effect | Spatial | 4 |
| `VoronoiPrism` | Voronoi Prism | effect | Stylize | 3 |
| `Watercolor` | Watercolor | effect | Stylize | 4 |
| `Wireframe` | Wireframe | generator | Geometry | 9 |
| `WireframeDepth` | Wireframe Depth | effect | Diagnostic | 8 |

<!-- END GENERATED: registered-node-index -->

---

## 3. Atoms by intent

### 3.1 Control-rate scalar plumbing

Free to evaluate (no GPU dispatch). The scalar wire graph runs every frame with negligible cost; use these for any modulation-shaped value.

| Display Name | Type ID | Purpose |
|---|---|---|
| Value | `node.value` | Constant scalar source — every outer-card slider routes through one |
| Math | `node.math` | Two-input scalar math (Add/Subtract/Multiply/Divide/Min/Max/Atan2/Sin/Cos); `b` ignored for unary ops |
| Affine Scalar | `node.affine_scalar` | `value * scale + offset` — collapses Value+Math+Value+Math chains |
| LFO | `node.lfo` | Low-frequency oscillator (`Musical` follows `beat`, `Free` follows `time`); Sine/Tri/Saw/Square/SH |
| Beat Gate | `node.beat_gate` | Beat-synced square 0/amount gate with `duty` cycle |
| Beat Ramp | `node.beat_ramp` | Per-beat attack envelope — snaps to 0 each beat, ramps to 1 over the first `attack` fraction; seek-safe |
| Trigger Gate | `node.trigger_gate` | Emit a single-frame pulse on integer-edge changes of an input scalar |
| Smoothing | `node.smoothing` | One-pole low-pass on a scalar (stateful) |
| Envelope Follower (AR) | `node.envelope_follower_ar` | Attack/release envelope from an impulse (stateful) |
| Compressor Envelope | `node.compressor_envelope` | Audio-compressor envelope path applied to a scalar signal level — log-domain program-dependent A/R + ratio compression toward a `target`, out is a gain multiplier in [0.1, 10.0] (stateful; AutoGain) |
| Envelope Decay | `node.envelope_decay` | Decay-only envelope (stateful) |
| Sample & Hold | `node.sample_and_hold` | Hold the last sampled input until next trigger (stateful) |
| Trigger Ease To | `node.trigger_ease_to` | Snap-and-glide on a scalar: on each trigger edge captures current visible as `prev` and the input as `curr`, then eases over `window_beats` beats via cubic ease-out (stateful) |
| Threshold (scalar) | `node.filter` (id `node.threshold`) | Pass-above-cutoff with hard / soft-knee mode (also wraps a Texture2D variant) |
| Frequency Ratio | `node.frequency_ratio` | Curated 10-row harmonic ratio table indexed by `trigger_count`, uniqueness-enforced |
| Cycle Table Row | `node.cycle_table_row` | Cycle through a curated `Table` of f32 rows; emits the selected row as `Array<f32>` |
| Clip Trigger Cycle | `node.clip_trigger_cycle` | Wraps `ClipTriggerCycle::step` for primitive-internal trigger→variant mapping |
| Clip Trigger Index | `node.clip_trigger_index` | Same as Cycle but emits the integer index directly |
| Inject Burst | `node.inject_burst` | One-shot scalar burst on trigger; decays over a frame window |

### 3.2 Coordinate fields

Procedural textures that emit per-pixel coordinates. Start most procedural graphs from one of these.

| Display Name | Type ID | Purpose |
|---|---|---|
| UV Field | `node.uv_field` | Per-pixel `(u, v, 0, 1)` in `[0, 1]² `texture-space |
| Centered UV | `node.centered_uv` | Same as UV Field but in `[-1, +1]²` aspect-corrected |
| Polar Field | `node.polar_field` | Per-pixel `(angle/τ, radius, 0, 1)` around a configurable center |
| Grid UV Field | `node.grid_uv_field` | Per-instance UVs as `Array<vec2<f32>>` for instanced rendering |

### 3.3 Procedural noise + texture sources

| Display Name | Type ID | Purpose |
|---|---|---|
| Simplex Noise 2D | `node.simplex_noise_2d` | 2D Ashima simplex, remapped `[0, 1]` to RGB |
| Simplex Field 2D | `node.simplex_field_2d` | 3D simplex sampled at `(uv*scale + offset, z)`, signed output in R |
| Simplex (per instance) | `node.simplex_per_instance` | Per-instance 3D simplex → `Array<f32>` |
| Perlin Noise 2D | `node.perlin_noise_2d` | Classic Perlin gradient noise (different aesthetic from simplex) |
| FBM 2D | `node.fbm_2d` | Octave-summed Perlin (fractional Brownian motion) |
| FBM (per instance) | `node.fbm_per_instance` | Bit-identical to `fbm_2d` but indexed by `Array<vec2<f32>>` |
| Hash Noise 2D | `node.hash_noise_field_2d` | Uncorrelated wang-hash white noise — grain, dust, LIC ink |
| Hash Field by Seed | `node.hash_field_by_seed` | Re-hash a value field's RG by an added scalar seed (Hash2 → RG, Hash1 → RGB) — per-cell randoms that re-roll each beat |
| Flow Field Noise | `node.flow_field_noise` | 2-channel flow vectors for advection (Watercolor-style) |
| Voronoi 2D | `node.voronoi_2d` | Worley/Voronoi — F1 (R), F2 (G), F2-F1 (B), per-cell stable hash (A). Foundation for stars, foam, cracked glass, embers, tiles. |
| Checkerboard | `node.checkerboard` | Binary checker pattern at configurable scale |
| Distance to Point | `node.distance_to_point` | Per-pixel distance to a configurable point in UV space |
| Dither Pattern | `node.dither_pattern` | Per-pixel ordered-dither / halftone threshold field — six algorithms (Bayer 8×8, Halftone, Lines, CrossHatch, Blue Noise, Diamond); pairs with `node.dither` |
| Basic Shape | `node.basic_shape` | Single-dispatch SDF — Square / Diamond / Octagon picked by static `shape` enum. Three instances + `mux_texture` gives runtime shape selection. |
| Color | `node.color` (id `node.brightness`) | Per-pixel luminance to RGB |

### 3.4 Per-pixel texture math

Compose these for arbitrary procedural fields.

| Display Name | Type ID | Purpose |
|---|---|---|
| Sin Term | `node.sin_term` | `sin((a*r + b*g + c) * freq + time * rate)` — one term of a sum-of-sines |
| Trig Texture | `node.trig_texture` | Per-pixel Sin / Cos / Tan with freq + phase. Both freq and phase have optional texture-shadow inputs (`freq_tex` / `phase_tex` — R channel sampled per pixel) — unlocks per-cell unique trig modulation when fed from per-cell-stable sources like `voronoi_2d.A` via `channel_mix`. |
| Abs Texture | `node.abs_texture` | Per-pixel `abs(rgb)` |
| Fract Texture | `node.fract_texture` | Per-pixel `fract(rgb)` |
| Power Texture | `node.power_texture` | Per-pixel `pow(rgb, exponent)` |
| Smoothstep Texture | `node.smoothstep_texture` | Per-pixel smoothstep contrast curve with low/high edges |
| Scale/Offset Texture | `node.scale_offset_texture` | Per-pixel affine `a*x + b` — the general re-range primitive |
| Field Combine | `node.field_combine` | `a*r + b*g + c` — project a 2-channel field onto a scalar |
| Gain | `node.gain` | Scalar-driven RGB multiplier (port-shadow on `gain`) |
| Invert | `node.invert` | Invert RGB, crossfade by `intensity` |
| Flash | `node.flash` | Modulate brightness by a scalar `amount` — Opacity / White / Gain mode; Strobe's apply half (wire beat_gate into `amount`) |

### 3.5 Color & tone

| Display Name | Type ID | Purpose |
|---|---|---|
| Clamp | `node.clamp_texture` | Per-pixel saturate to [min, max] — the texture-side counterpart of `array_math::Clamp01` |
| Channel Mix | `node.channel_mix` | Per-pixel 4×4 RGBA matrix transform. Default = identity. Use to swizzle channels (A→R for reading cell_hash as a control signal), pull luma, isolate a single channel, or pre-tint for halation-style chains. |
| Levels | `node.levels` | Fused per-channel `pow(clamp(in*scale+offset, lo, hi), gamma)` — collapses scale_offset → clamp → power into one dispatch |
| Contrast | `node.contrast` | Pivot-around-0.5 contrast `(c-0.5)*contrast+0.5`; HDR-safe affine (no gamma NaN) |
| Saturation | `node.saturation` | Luma-based saturation `mix(luma, c, saturation)` — pulls toward perceptual grey (the Color Grade look) |
| Hue / Saturation | `node.hue_saturation` | HSV adjust — rotate hue (deg), scale saturation + value; Color Grade composes from this |
| Colorize | `node.colorize` | Selective tint toward a hue, masked per-pixel by brightness × neutrality × focus (duotone toward highlights) |
| Posterize | `node.posterize` | Quantize each RGB channel to `levels` discrete steps; Dither composes from this |
| Film Grain | `node.film_grain` | Multiplicative white-noise grain `src*(1-amount*(1-noise))` — paper-texture pass of Watercolor |
| Gradient Ramp | `node.gradient_ramp` | N-stop (≤16) 1D gradient / LUT generator with last-segment HDR extrapolation; luminance LUT for `color_lut` |
| HDR Retention Mix | `node.hdr_retention_mix` | Preserve a reference texture's above-1.0 highlight energy through a compressed texture's gain adjustment |
| Color LUT | `node.color_lut` | 1D LUT remap via luminance index |
| Chroma Key | `node.chroma_key` | Per-pixel RGB-distance mask to a target colour |
| Chromatic Displace | `node.chromatic_displace` | Per-channel UV displacement by a vector field |
| Tone Map | `node.tone_map` | HDR → SDR/PQ/EDR with ACES / AgX / Khronos Neutral curves |
| Reinhard Tone Map | `node.reinhard_tone_map` | Extended Reinhard, SDR-only; bit-matches FluidSim display |

### 3.6 Image transforms

The UV-warp family below is `coordinate-field → node.remap → node.mix` (TouchDesigner's Remap-TOP shape): a coordinate generator emits per-pixel sample UVs, `node.remap` resamples the source at them, `node.mix` crossfades. This visible graph replaced the fused whole-effect kernels — `radial_fold_uv` ⇐ Kaleidoscope, `mirror_fold_uv` ⇐ Mirror / QuadMirror, `uv_strip_clamp` ⇐ Edge Stretch, `radial_offset_field` + `chromatic_displace` ⇐ Chromatic Aberration. The affine half (translate/scale/rotate) stays in `node.affine_transform`.

| Display Name | Type ID | Purpose |
|---|---|---|
| Remap | `node.remap` | Resample `source` at per-pixel UVs from a coordinate field (TD Remap TOP); Absolute / Relative field mode, Clamp/Repeat/Mirror wrap. The generic UV-warp atom |
| Affine Transform | `node.affine_transform` | Three-scalar-port affine — port-shadow demo for translate_x/y + rotation |
| Rotate 2D | `node.rotate_2d` | Rotate a 2D coordinate field around the origin |
| Radial Fold UV | `node.radial_fold_uv` | Kaleidoscope coordinate generator — folds the plane into N mirrored wedges and emits the sample UV |
| Mirror Fold UV | `node.mirror_fold_uv` | Mirror/fold coordinate generator (Identity / Mirror / MirrorX/Y / FlipY / QuadMirror / Fold modes) — emits the folded sample UV |
| UV Strip Clamp | `node.uv_strip_clamp` | Edge-stretch coordinate generator — clamps UV to a center strip (Horiz/Vert/Both) so resampling stretches edge pixels outward |
| Radial Offset Field | `node.radial_offset_field` | Directional displacement field (Radial outward-with-falloff or Linear at `angle`) — feeds chromatic_displace / uv_displace_by_flow / texture_advect |
| Block Displace Field | `node.block_displace_field` | Per-block random UV-offset field (datamosh building block) — emits gated `offset` (RG) + per-block `hash`; feed into `node.remap` (Relative) |
| Scanline Jitter Field | `node.scanline_jitter_field` | Per-row random horizontal-offset field (VHS tearing) — gated `offset`; feed into `node.remap` (Relative) |
| Slope Displace | `node.slope_displace` | Emboss-style displacement along a soft-light luminance Sobel gradient — Watercolor's pigment-pooling edge pull |
| Mirror Axis | `node.mirror_axis` | Sample input at UVs mirrored across a line through center at `angle` — single-axis 2-fold symmetry (one half visible, other half is mirror) |
| UV Displace by Flow | `node.uv_displace_by_flow` | Sample texture at UVs displaced by a 2-channel flow field |

### 3.7 Spatial filters

| Display Name | Type ID | Purpose |
|---|---|---|
| Gaussian Blur | `node.separable_gaussian` (id `node.gaussian_blur`) | Separable Gaussian, one axis per pass |
| Gaussian Blur (variable width) | `node.gaussian_blur_variable_width` | Per-pixel kernel width from a `width` texture (DoF, masked blur) |
| 3D Separable Blur | `node.blur_3d_separable` | Single-axis Gaussian on a Texture3D (volumetric) |
| Downsample | `node.downsample` | Integer-factor box-filter — pyramid front |
| Convolution 2D 9-tap | `node.convolution_2d_9tap` | General 3×3 kernel — Sobel, Laplacian, emboss, custom |
| Sharpen | `node.sharpen` | One-knob Laplacian unsharp mask |
| Edge Detect | `node.edge_detect` | Sobel 3×3 + smoothstep threshold + crossfade |

### 3.8 Compositing

| Display Name | Type ID | Purpose |
|---|---|---|
| Mix | `node.compose` (id `node.mix`) | Blend two textures — Lerp/Screen/Add/Max/Multiply/Difference/Overlay/Divide. Divide guards against b≈0. |
| Masked Mix | `node.masked_mix` | Per-pixel weighted blend driven by mask.r |
| Wet/Dry | `node.wet_dry_mix` (id `node.wet_dry`) | Crossfade processed against original |
| Texture Sum 5 | `node.texture_sum_5` | Weighted sum of 5 textures — collapses long Mix(Add) chains |
| Pack RGBA | `node.pack_channels` | Combine four single-channel textures into one RGBA by reading `.r` of each input into the matching output channel — the recompose-after-atomic-per-channel-processing atom |
| Vignette | `node.vignette` | Soft fade-to-black border — Circle / Ellipse / Rectangle |

### 3.8a Mask sources

SDF / gradient mask generators (RGB = mask value, smoothstep falloff). Pair downstream with `masked_mix`, or `node.invert` to flip polarity.

| Display Name | Type ID | Purpose |
|---|---|---|
| Box Mask | `node.box_mask` | Rotated rectangular SDF mask (Chebyshev) — band masks for tilt-shift / scanlines / letterboxes |
| Ellipse Mask | `node.ellipse_mask` | Rotated elliptical SDF mask — industry-standard masking convention |
| Linear Gradient | `node.linear_gradient` | Directional 0→1 ramp in UV space — fades / wipes; pairs with masked_mix |

### 3.9 Stateful temporal

State lives in the primitive via `extra_fields:` + `state_lifecycle`. StateStore keys by `(owner_key, node_id)` — fresh on graph rebuild.

| Display Name | Type ID | Purpose |
|---|---|---|
| Feedback | `node.temporal` (id `node.feedback`) | Previous-frame texture accumulation with `amount`, `zoom`, `rotation`, mode |
| Array Feedback | `node.array_feedback` | One-frame delay for `Array<Particle>` — closes per-frame loops without graph cycles |
| Smoothing (scalar) | `node.smoothing` | Listed under control-rate; also valid stateful temporal |
| Envelope Follower / Decay / Sample & Hold | (see §3.1) | Scalar-side temporal state |

### 3.10 Texture → scalar bridges

One-frame readback latency. Pair with `Gain`, `Math`, `Feedback.amount`, etc. for image-driven modulation.

| Display Name | Type ID | Purpose |
|---|---|---|
| Brightness (scalar) | `node.luminance` | Average Rec.709 luma of the whole image |
| Peak | `node.peak` | Maximum Rec.709 luma across the image |
| Color Sample | `node.color_sample` | Region-averaged RGB at a configurable UV + luma |
| Texture Dimensions | `node.texture_dimensions` | Read input texture `width` / `height` / `aspect` as scalars — no GPU dispatch, zero latency (feed aspect-correction downstream) |

### 3.11 Gradient / vector-field atoms

| Display Name | Type ID | Purpose |
|---|---|---|
| Gradient (central diff) | `node.gradient_central_diff` | Half-difference gradient `(dx, dy)` of a single channel. `scale_mode`: Texel (default) or UV (multiplies by dim/2 per axis). `wrap_mode`: Clamp (default) or Repeat (toroidal — fluid sims). |
| Heightmap to Normal | `node.heightmap_to_normal` | Scalar height → tangent-space normal map via central-diff |
| Length (vec2) | `node.length_vec2` | `length(in.rg)` as a scalar field — vec2 magnitude atom |
| Normalize (vec2) | `node.normalize_vec2` | Safe-normalize RG as a 2D direction field |
| Rotate vec2 (Angle) | `node.rotate_vec2_by_angle` | Per-pixel vec2 rotation by an arbitrary port-shadowed angle (radians); default PI/2. Legacy `node.rotate_vec2_90` type-ID aliases here. |
| Array Unpack vec2 | `node.array_unpack_vec2` | Decompose `Array<vec2<f32>>` into two `Array<f32>` channels |
| Canvas Area Scale | `node.canvas_area_scale` | `(width * height) / reference_area` — resolution-aware brightness compensation |

### 3.12 PBR shading atoms

All operate on tangent-space normal maps and a directional light. Sum the additive ones via `node.mix` mode=Add.

| Display Name | Type ID | Purpose |
|---|---|---|
| Lambert Directional | `node.lambert_directional` | Diffuse shading from normal + light + ambient (base term) |
| Blinn Specular | `node.blinn_specular` | Blinn-Phong specular (additive) |
| Fresnel Rim | `node.fresnel_rim` | Fresnel edge highlight (additive) |
| Matcap Two-Tone | `node.matcap_two_tone` | Cross-axis 4-colour matcap from a normal map |
| Bake Equirect Envmap | `node.bake_equirect_envmap` | Procedural HDR studio environment map at configurable resolution (one-shot persistent output, equirect layout) — wire into `node.render_3d_mesh`'s `envmap` for PBR-IBL |

#### Material wires (consumed by the mesh renderers)

One `Material` per node, wired into `render_3d_mesh` / `render_instanced_3d_mesh`. See [MATERIAL_SYSTEM_DESIGN.md](MATERIAL_SYSTEM_DESIGN.md).

| Display Name | Type ID | Purpose |
|---|---|---|
| Unlit Material | `node.unlit_material` | Flat-colour material — no lighting / shadow term; renderer writes base + emission directly. No `light` input required |
| Phong Material | `node.phong_material` | Lambert diffuse + Blinn-Phong specular + ambient floor — cheap lit baseline (requires a `light`) |
| Cel Material | `node.cel_material` | Cel-shaded — Lambert N·L quantized into `cel_bands` discrete bands (the DigitalPlants look; requires a `light`) |

*Photoreal PBR (Cook-Torrance + IBL) lives inside `node.render_3d_mesh`'s `node.pbr_material`, not as standalone wireable atoms — the standalone `cook_torrance_specular` / `equirect_envmap_sample` were removed 2026-05-30 (zero references; below the level any tool exposes, cf. Blender's Principled BSDF). The à-la-carte shading atoms above stay for stylized / NPR looks (no canonical answer to compose).*

### 3.13 Flow & fluid

Per-frame fluid-sim primitives. Pair upstream with seed + downstream with scatter/resolve.

| Display Name | Type ID | Purpose |
|---|---|---|
| Texture Advect | `node.texture_advect` | Backward semi-Lagrangian advection by a velocity field |
| LIC Integrate | `node.lic_integrate` | Line Integral Convolution — flow visualisation streamlines |
| Gradient (Central Diff 3D) | `node.gradient_central_diff_3d` | 6-tap central-diff gradient of a density Texture3D → vec3 Texture3D (toroidal wrap, ×0.5). 3D sibling of `gradient_central_diff` |
| Curl + Slope Force 3D | `node.curl_slope_force_3d` | `cross(gradient, ref_axis)*curl + gradient*slope` → vec3 force Texture3D. Pairs with `gradient_central_diff_3d` (the decomposed FluidSim3D force field) |
| Sample Texture 3D at Particles | `node.sample_texture_3d_at_particles` | Per-particle trilinear sample of a vec3 Texture3D at position.xyz → `Array<[f32;3]>` forces. 3D sibling of `sample_texture_at_particles` |
| Simplex Noise Force 3D at Particles | `node.simplex_noise_force_3d_at_particles` | 3-plane density-adaptive simplex advection added to the force buffer |
| Diffuse Force 3D at Particles | `node.diffuse_force_3d_at_particles` | Density-weighted incoherent random kick added to the force buffer |
| Container Repel Force 3D | `node.container_repel_force_3d` | Soft SDF boundary cushion (Cube/Sphere/Torus) added to the force buffer (pre-integration) |
| Euler Step Particles 3D | `node.euler_step_particles_3d` | `position.xyz += forces * speed * (dt*60)`. 3D sibling of `euler_step_particles` |
| Container Bounds 3D | `node.container_bounds_3d` | Post-integration hard containment: toroidal wrap (None) or SDF reflect+clamp. 3D sibling of `wrap_particles_torus` |
| Flatten to Camera Plane | `node.flatten_to_camera_plane` | Compress particles toward the camera viewing plane (reads `cam.fwd`) |
| Apply Radial Burst (Particles) | `node.apply_radial_burst_to_particles` | Per-particle radial+tangent impulse around a point — FluidSim2D inject path |
| Apply Radial Burst 3D (Particles) | `node.apply_radial_burst_3d_to_particles` | Per-particle 3D injection burst around 4 tetrahedron zones + vortex ring — FluidSim3D inject path |
| Scatter Particles Camera | `node.scatter_particles_camera` (alias `node.fluid_project_scatter_2d`) | 3D particles → 2D u32 accumulator via Camera projection. Sibling to `scatter_particles` / `scatter_particles_3d` |
| Sample Volume 2D | `node.sample_volume_2d` | Sample a Texture3D as 2D slice/projection |

### 3.14 3D + 4D geometry pipeline

| Display Name | Type ID | Purpose |
|---|---|---|
| Orbit Camera | `node.camera_orbit` | Orbit-style perspective `Camera` from five port-shadowed scalars (orbit/tilt/distance/fov_y/look_y); also emits `pos_x/y/z` for PBR shading. One wire replaces N per-renderer camera params |
| Light | `node.light` | Single Sun / Point light → `Light` wire consumed by shading atoms + shadow-aware mesh renderers; all params port-shadowed (one node per light) |
| Generate Grid Mesh | `node.generate_grid_mesh` | NxM grid of `MeshVertex` in XZ plane — heightmap-displaced surfaces |
| Generate Cube Mesh | `node.generate_cube_mesh` | Unit cube as 36 `MeshVertex` triangle-list |
| Polytope Vertices | `node.polytope_vertices` | One of the five Platonic solids as `Array<MeshVertex>`, baked to magnitude 0.25 (curated-enum GPU dispatch) |
| Polytope Edges | `node.polytope_edges` | Wireframe edge topology of the selected Platonic solid as `Array<EdgePair>` (curated CPU lookup) — pair with `polytope_vertices` on the same `shape` scalar |
| Generate Tesseract Vertices | `node.generate_tesseract_vertices` | 16 4D corners + 32 edges for 4D wireframe (hypercube bit-flip topology — closed mathematical structure, hand-typed coords + const-fn edge table) |
| Generate Grid UV | `node.generate_grid_uv` | Pattern-CHOP-of-a-grid: emit two `Array<f32>` (u, v) sampling `[0, u_max) × [0, v_max)` at `n` steps each, flattened row-major (`n²` entries). The (u, v)-parametric authoring atom — pair with `array_math` + `pack_vec4` + `edges_from_grid_uv` to author any parametric surface in pure JSON |
| Pack Vec4 | `node.pack_vec4` | Zip four `Array<f32>` (x, y, z, w) into `Array<Vec4Vertex>`. The 4D analogue of `pack_curve_xy`; pure structural transformation (no scale bake — per-shape magnitude is applied upstream via `array_math(ScaleOffset)`) |
| Edges From Grid UV | `node.edges_from_grid_uv` | u-wrap + v-wrap wireframe edge topology for an `n × n` parametric grid as `Array<EdgePair>` (`2n²` edges). Topology counterpart of `polytope_edges` for any (u, v)-sampled surface (torus, Klein, sphere, terrain) |
| Generate Instance Transforms | `node.generate_instance_transforms` | Procedural `Array<InstanceTransform>` (grid/ring/spiral/random) |
| Nested Cubes Geometry | `node.nested_cubes_geometry` | Curated instanced-cube layout for NestedCubes preset |
| Displace Mesh | `node.displace_mesh` | Perturb mesh Y from a heightmap texture, per-vertex UV sample |
| Triangulate Grid | `node.triangulate_grid` | NxM positions → triangle-list with finite-difference normals |
| Rotate 3D / 4D | `node.rotate_3d`, `node.rotate_4d` | Euler XYZ; stereo XY/ZW/XW for 4D |
| Project 3D / 4D | `node.project_3d`, `node.project_4d` | Orthographic / perspective projection to curve-space |
| Cylinder Wrap Field | `node.cylinder_wrap_field` | Lift `Array<vec2>` onto a cylinder surface as `Array<InstanceTransform>` |
| Torus Wrap Field | `node.torus_wrap_field` | Same shape for a torus |
| Render 3D Mesh | `node.render_3d_mesh` | Render `Array<MeshVertex>` triangle list — Lambert + ambient + orbit-camera params. Also emits `world_pos` + `world_normal` G-buffer outputs (always — TouchDesigner / Blender deferred-shading shape) for downstream screen-space PBR / SSAO / SSR atoms |
| Render Instanced 3D Mesh | `node.render_instanced_3d_mesh` | Render N copies of a base mesh via `Array<InstanceTransform>` |
| Render Lines | `node.render_lines` | Anti-aliased capsule line segments from `Array<CurvePoint>`; optional `edges` input |
| Digital Plants Render | `node.digital_plants_render` | Two-pass shadow + instanced cel-shaded cubes (DigitalPlants-specific) |

### 3.15 2D curves

| Display Name | Type ID | Purpose |
|---|---|---|
| Generate Range | `node.generate_range` | Pattern-CHOP linspace: `Array<f32>` of N samples over `[start, end]`. `end_inclusive` toggles between closed (Lissajous) and exclusive (regular N-gons) sampling; `active_count` port-shadows the runtime sample count for variable-N curves |
| Pack Curve XY | `node.pack_curve_xy` | Zip two `Array<f32>` (x, y) into `Array<CurvePoint>`; folds the `PROJ_SCALE = 0.25` screen-fit constant. Curve-pipeline counterpart to `array_unpack_vec2` |
| Consecutive Edges | `node.consecutive_edges` | Synthesise polyline edge topology `[(0,1), (1,2), …]` from a vertex count; optional closing `(N-1, 0)` edge. Inactive tail is `EdgePair::SENTINEL` for variable-N polygons |
| Replicate Polyline Rings | `node.array_replicate_polyline_rings` | Stack K transformed copies of a polyline (outline + edges) — per-ring uniform scale on points, per-ring index shift on edges (sentinel-preserving). The concentric / stacked-curve atom |
| Connect Nearest | `node.array_connect_nearest` | For each item in a `Channels[X, Y, WIDTH, HEIGHT]` array find its nearest neighbour within `max_distance` and emit an `EdgePair` — sparse nearest-neighbour graph for `render_lines` connection-line viz |

### 3.16 Particle / instance simulation

| Display Name | Type ID | Purpose |
|---|---|---|
| Seed Particles | `node.seed_particles` | Wang-hash uniform `Array<Particle>` seed (EveryFrame or OnceOnReset) |
| Seed Particles from Texture | `node.seed_particles_from_texture` | Seed particles weighted by an input texture's brightness |
| Sample Texture at Particles | `node.sample_texture_at_particles` | Per-particle bilinear sample of a Texture2D at `position.xy` → `Array<vec2<f32>>` of RG samples |
| Euler Step Particles | `node.euler_step_particles` | Apply `position.xy += forces * speed * (delta * 60)` per live particle. Aliased in/out. |
| Wrap Particles (Torus) | `node.wrap_particles_torus` | Per-particle toroidal wrap `position.xy = fract(position.xy + 1)`. Cyclic-boundary policy atom. |
| Diffuse Particles | `node.array_diffuse_particles` | Hash-based random kick on `Particle.velocity` (generic Brownian noise — ODE-state diffusion) |
| Anti-Clump Particles | `node.anti_clump_particles` | Modulator-weighted hash kick on `Particle.position.xy` — optional scalar Texture2D `strength_modulator` concentrates the kick (FluidSim wires density; works with any scalar map). Unwired = plain uniform Brownian jitter. |
| Simplex Noise Force at Particles | `node.simplex_noise_force_at_particles` | Per-particle 2D simplex noise force added in-place to an `Array<vec2<f32>>` buffer. Optional scalar Texture2D `amplitude_modulator` adds capped density-style amplitude boost (legacy density-adaptive noise). Resolution-independent replacement for per-pixel simplex noise texture chains. |
| Radial Burst Force Field | `node.radial_burst_force_field` | Per-pixel vec2 force texture for a radial+tangent impulse around a point with falloff envelope. Sum into a velocity field for "impulse around a point" particle behaviour. |
| Scatter Particles | `node.scatter_particles` | Atomic-add splat into 2D u32 accumulator (Wrap / Discard boundary) |
| Scatter Particles 3D | `node.scatter_particles_3d` | Same shape for `Texture3D` accumulator |
| Resolve Accumulator | `node.resolve_accumulator` | u32 grid → float Texture2D |
| Resolve 3D Accumulator | `node.resolve_3d_accumulator` | u32 grid → float Texture3D |
| Scalar Array Accumulator | `node.scalar_array_accumulator` | Stateful running sum of an `Array<f32>` |
| Array Math | `node.array_math` | Element-wise math on `Array<f32>` (Add/Sub/Mul/Div/Min/Max/ScaleOffset/Shape/MirrorRamp/Clamp01/Abs/Sin/Cos/Mix) |
| Instance Position Jitter | `node.instance_position_jitter` | Per-instance position offset from a noise field |
| Instance Rotation Jitter | `node.instance_rotation_jitter` | Per-instance rotation jitter |
| Lerp Instance Fields | `node.lerp_instance_fields` | Per-field interpolation between two `Array<InstanceTransform>` |
| Neighbor Smooth | `node.neighbor_smooth` | 5-point cross-neighborhood smoothing on an NxN grid of InstanceTransforms |

### 3.17 Routing

Variadic N-way selectors driven by a scalar.

| Display Name | Type ID | Purpose |
|---|---|---|
| Mux Scalar | `node.mux_scalar` | 8-way scalar selector |
| Mux Texture | `node.mux_texture` | 8-way `Texture2D` selector |
| Mux Array | `node.mux_array` | 8-way `Array<T>` selector |

### 3.18 Native / FFI / host-side sources

These wrap native plugins, CPU work, or background workers as primitives.

| Display Name | Type ID | Purpose |
|---|---|---|
| Depth Estimate (MiDaS) | `node.depth_estimate_midas` | Monocular depth via FFI plugin — background worker, ~2-3 frame latency |
| Person Segment | `node.person_segment` | Human/person segmentation via native plugin — R=G=B = person probability mask (~2-3 frame latency); same channel pack as depth_estimate_midas |
| Blob Detect (FFI) | `node.blob_detect_ffi` | Sparse blob detection — emits `Array<Blob>` |
| Blob Overlay Render | `node.blob_overlay_render` | Draws blob bounding boxes |
| Optical Flow | `node.optical_flow_estimate` | Per-pixel optical flow vectors |
| Track Persist | `node.track_persist` | Greedy nearest-neighbour identity tracking with grace-period retention on `Channels[X, Y, WIDTH, HEIGHT]` detections — stable IDs across frames (prereq for one_euro_filter) |
| One Euro Filter | `node.one_euro_filter` | Adaptive temporal low-pass (1€ filter) on a Channels array — heavy smoothing when still, responsive when fast; per-channel per-sample |
| Render Filled Rects | `node.render_filled_rects` | Instanced filled-rectangle overlay from a `Channels[X, Y, WIDTH, HEIGHT]` array (additive) — gauges, debug regions, VU meters |
| Render Value Overlay | `node.render_value_overlay` | Bitmap-font numeric labels at multiple positions (5×7 atlas; Index/Hex/Coord/Float3 format) — diagnostic HUDs |
| Image Folder | `node.image_folder` | Scrub through a folder of images via a position scalar |
| Render Text | `node.render_text` | CoreText glyph rasterizer wrapped as a primitive — composite a text string into the output with position / scale / aspect / alignment |
| Auto Gain Apply | `node.auto_gain_apply` | GPU side of AutoGain — pairs with the CPU envelope follower |

### 3.19 WGSL escape hatch

Reserved for genuinely irreducible kernels (see DECOMPOSING_GENERATORS §5 before reaching).

| Display Name | Type ID | Purpose |
|---|---|---|
| WGSL Compute | `node.wgsl_compute` | Naga-introspected user compute kernel — ports/uniforms/array fields derived from the shader source; arbitrary texture + `Array<T>` in/out. Replaces the three fixed-arity `wgsl_compute_*in_*tex` variants. First consumers: BlackHole, ComputeStrangeAttractor, FluidSim3D |

---

## 4. Effects — named visual looks

**Effects are JSON presets, not palette nodes.** Every effect now ships as a decomposed atom graph in [`assets/effect-presets/`](../crates/manifold-renderer/assets/effect-presets/), runs `system.source → atoms → system.final_output`, and is drillable in the graph editor (open the effect, see its atoms, rewire them). The pre-migration model — effects as monolithic `shader` / `composite` / `bundle` nodes in the palette — is gone: the fused legacy effect-monolith bundles were **deleted on 2026-05-30**, and the named-look kernels (Kaleidoscope, Quad Mirror, Edge Stretch, Chromatic Aberration, Color Grade, Infrared, Plasma, Bloom, Strobe, …) were replaced by composable atoms (the §3.6 UV-warp family, the §3.5 colour atoms, etc.).

**One legacy wrapper remains: Wireframe Depth** (`node.wireframe_depth`, `WIREFRAME_DEPTH_TYPE_ID`) still wraps the legacy `WireframeDepthFX: PostProcessEffect` Rust impl. It is the lone remaining fused effect node and an active decomposition target — its 48-node `WireframeDepthGraph.json` atom-graph replacement is in flight (depth + person-segment DNN atoms + edge_detect + wireframe primitives). Until that lands, `WireframeDepth.json` is a thin wrap of the legacy node and `WireframeDepthGraph.json` is the parallel atom-graph build-out.

The effect presets are listed in §5.

---

## 5. Effect presets

26 JSON files at [`assets/effect-presets/`](../crates/manifold-renderer/assets/effect-presets/). Each is a decomposed atom graph (drillable in the editor); the atom composition is noted. The only thin-wrap-of-a-legacy-node is `WireframeDepth` (wraps `node.wireframe_depth`); `WireframeDepthGraph` is its in-flight atom-graph replacement.

| Preset | Atom shape |
|---|---|
| AutoGain | `luminance` → `compressor_envelope` → `gain` → `hdr_retention_mix` → `wet_dry` |
| BlobTracking | `blob_detect_ffi` → `track_persist` → `one_euro_filter` → `array_connect_nearest` → `render_value_overlay`; `wgsl_compute` ×8 + `affine_scalar` ×2 |
| Bloom | `threshold` → `downsample` → `blur` → `mix` |
| ChromaticAberration | `radial_offset_field` + `math` → `chromatic_displace` → `mix` |
| ColorCompass | 4× `color_sample` → `math` → `smoothing` → `affine_transform` — texture-to-scalar bridge closing the loop into image transform |
| ColorGrade | `contrast` → `saturation` → `hue_saturation` → `colorize` → `gain` → `clamp_texture` → `mix` |
| DepthOfField | `depth_estimate_midas` / `box_mask` / `ellipse_mask` + CoC math → `gaussian_blur_variable_width` ×2 → `masked_mix` |
| Dither | `dither_pattern` → `dither` |
| EdgeGlow | `edge_detect` standalone |
| EdgeStretch | `uv_strip_clamp` → `remap` → `mix` |
| Glitch | `block_displace_field` + `scanline_jitter_field` + `radial_offset_field` → `remap` → `chromatic_displace` + per-block `invert` via `masked_mix`, gated by `value`/`math`/`mix` |
| HdrBoost | `threshold` → `gain` → `math` ×2 → `mix` |
| Infrared | `gradient_ramp` ×10 → `mux_texture` → `color_lut` (thermal palette as N-stop ramps) |
| InvertColors | `invert` standalone |
| Kaleidoscope | `radial_fold_uv` → `remap` → `mix` (verbatim fold port of the legacy bundle) |
| Mirror | `mirror_fold_uv` → `remap` → `mix` |
| NodeGraphTest | test fixture (`mix`) |
| QuadMirror | `centered_uv` → `abs_texture` → `scale_offset_texture` → `remap` → `mix` |
| SoftFocusGraph | `blur` → `mix` |
| Strobe | `beat_gate` → `flash` |
| StylizedFeedback | `feedback` → `affine_transform` → `gain` → `vignette` → `mix` |
| Transform | `affine_transform` standalone |
| VoronoiPrism | `voronoi_2d` → `hash_field_by_seed` ×2 + `beat_ramp` → `uv_strip_clamp` → `remap` → per-cell beat-driven `mix` composite |
| Watercolor | `flow_field_noise` → `uv_displace_by_flow` → `slope_displace` → `feedback` + `blur` ×2 + `masked_mix` |
| WireframeDepth | thin wrap of `node.wireframe_depth` (legacy `PostProcessEffect`) |
| WireframeDepthGraph | in-flight atom-graph decomposition: `depth_estimate_midas` + `person_segment` + `optical_flow_estimate` + `wgsl_compute` ×13 + `feedback` ×5 + math/value scaffolding |

---

## 6. Generators

All shipping generators are JSON-defined sub-graphs at [`assets/generator-presets/`](../crates/manifold-renderer/assets/generator-presets/), running from `system.generator_input` to `system.final_output`. Zero `inventory::submit!` generators remain; [`crates/manifold-renderer/src/generators/`](../crates/manifold-renderer/src/generators/) is now runtime infrastructure only (loader, registry, mesh/line pipelines, math, stateful base).

### 6.1 JSON-defined

| Preset | Topology shape |
|---|---|
| ApricotWeather | Scene 1 — imported glTF scan composited via `render_scene`, one object per glTF material (mirrors `node_graph::gltf_import`'s per-material-object shape, not a merged mesh): 3× (`gltf_mesh_source(material_index=k)` → `bend_mesh(sway, base-anchored)` → `phong_material` + `gltf_texture_source` as base color), one shared `transform_3d` offset (combined bbox center) so the 3 materials stay coherent, `grid_mesh`→`make_triangles` ground plane, `node.atmosphere` fog, one shadow-casting `node.light` orbited by `value`→`math(Cos/Sin)`→`scale_offset_value` off a `sunAngle` hub, `orbit_camera`. A 4th glTF material (24-vertex reference cube) is excluded from the graph entirely — curation, not a bug. |
| BasicShapes | trigger-cycled SDF shapes, atomized: `clip_trigger_index` (variant cycle, modulus mux'd 3/6/3 on fill) + `math(Modulo/Divide/Floor)` derive `shape_idx`/`rot_step`/`is_wireframe`; 8-row `mux_scalar` table → signed rotation snap; `trigger_ease_to(window_beats=0.25)` glides between snaps over a quarter beat; three `basic_shape` instances (Square / Diamond / Octagon) → `mux_texture` selected by shape_idx. Shape selection is graph-visible; rotation-easing atom is generic (any snap-on-trigger glide). |
| BlackHole | Kerr black hole with relativistic geodesic lensing: 4× `wgsl_compute` (deflection bake → 3 tex out; Schwarzschild orbit integrator with aliased `Array<Particle>`; polar+hemisphere particle splat with dual atomic accums; cinematic compositor reading deflection + polar density + sky) + `seed_particles` (active_count=0 → simulate self-seeds) + `resolve_accumulator` ×2 + `gaussian_blur` ×10 (deflection H/V ×3 + polar density H/V ×2) + `affine_scalar` ×2 (deg→rad) + `math` (Reciprocal for scale→uv_scale). First consumer of the naga-introspected dynamic escape hatch. |
| ComputeStrangeAttractor | particle sim, atomized onto `wgsl_compute`: `seed_particles(OnceOnReset) → wgsl_compute(attractor_simulate — switch on attractor_type for Lorenz/Rössler/Aizawa/Thomas/Halvorsen, RK2 substeps + first-frame init/warmup + NaN guard, integrate + project bundled in one dispatch) → array_diffuse_particles → scatter_particles(Discard) → resolve_accumulator → reinhard_tone_map`. Adding a new attractor is a JSON edit (append a `case` to the switch + entries to the per-attractor center/scale/dt tables). clip_trigger via `clip_trigger_cycle` + `mux_scalar` (manual vs trigger-driven). Brightness compensated by canvas_area_scale. |
| ConcentricTunnel | mux'd polygon + ring stacker, fully atomized: `mux_scalar` ×many (N selection + trigger-mode gating + cycle [3,4,5,6,8,12]) → `generate_range(end_inclusive=false, active_count=N)` → `array_math(Cos/Sin + ScaleOffset)` ×4 → `pack_curve_xy(scale=4.0 cancels PROJ_SCALE)` → outline; `consecutive_edges(closed=true, count=N)` → edges; per-ring scales via `generate_range(0..15) → math(Floor/Sub/Mul)` + `array_math(ScaleOffset)` → `array_replicate_polyline_rings` → `render_lines`. Polygon math is graph-visible; the shipped atoms are reusable for any closed parametric curve. |
| DigitalPlants | instanced 3D mesh with procedural layout: `grid_uv_field` → `simplex_per_instance` + `fbm_per_instance` → `cylinder_wrap_field` / `torus_wrap_field` → instance jitters → `neighbor_smooth` → `digital_plants_render` |
| Duocylinder | 4D parametric-surface graph: `generate_grid_uv(n=24, [0,TAU)²) → array_math(Cos|Sin) × 4 axes → array_math(ScaleOffset, 0.176776695) × 4 → pack_vec4 → rotate_4d → project_4d → render_lines`; `edges_from_grid_uv(n=24)` wires the u/v-wrap topology into `render_lines.edges`. The `generate_grid_uv` + `array_math` + `pack_vec4` + `edges_from_grid_uv` family authors any (u, v)-parametric surface without a per-shape Rust atom |
| FluidSim2D | particle fluid sim: `fluid_seed` → `fluid_simulate` → `scatter_particles` → `resolve_accumulator` → `feedback` → `downsample` → `gaussian_blur` ×4 → `fluid_gradient_rotate` → `reinhard_tone_map` |
| FluidSim3D | volumetric particle fluid sim (fully atom-decomposed): `seed_particles` → `wgsl_compute` (8-pattern seed) → `array_feedback` → `scatter_particles_3d` → `resolve_3d_accumulator` → `blur_3d_separable` ×3 (density) → `gradient_central_diff_3d` → `curl_slope_force_3d` → `blur_3d_separable` ×3 (field) → per-particle chain (`sample_texture_3d_at_particles` → `simplex_noise_force_3d_at_particles` → `diffuse_force_3d_at_particles` → `container_repel_force_3d` → `euler_step_particles_3d` → `container_bounds_3d` → `flatten_to_camera_plane` → `apply_radial_burst_3d_to_particles`) → `scatter_particles_camera` → `resolve_accumulator` → `reinhard_tone_map`, with `camera_orbit` + `inject_burst` + `clip_trigger_cycle` drivers |
| Lissajous | parametric curve, fully atomized: `lfo` ×3 + `frequency_ratio` + `mux_scalar` ×2 → per-axis `math(Floor/Ceil/Subtract)` bracket + `generate_range` → `array_math(ScaleOffset+Sin)` ×4 + `array_math(Mix)` ×2 → `pack_curve_xy` → `render_lines`. The TouchDesigner Pattern→Math→Function→Merge→To-SOP shape; bracket-interp is graph-visible. |
| MetallicGlass | feedback-displacement metallic surface, fully atomized: `simplex_field_2d` + `scale_offset` → `feedback` ping-pong with `mix Difference`+`mix Lerp 0.98` → `gaussian_blur` H/V → split into (height/levels chain) and (`mirror_axis`+`convolution_2d_9tap`×2+`pack_channels`+`length_vec2` Sobel chain). Geometry: `generate_grid_mesh` → `displace_mesh(height=height_levels)` → `triangulate_grid` → `render_3d_mesh` (forward PBR pass). Shading: `gain(height × displace) → heightmap_to_normal(coord_space=WorldYUp, aspect=system.aspect)` → `normal_map`; `scale_offset_texture(edge, scale=0.15, offset=0.05)` → `roughness_map`; `bake_equirect_envmap` → `envmap`. `render_3d_mesh`'s `pbr_material` does Cook-Torrance (D_GGX × G_Smith × F_Schlick) + IBL internally, sampling normal/roughness at mesh UV and writing linear colour straight to `final_output` (no standalone specular / envmap-sample / tone-map nodes — refactored 2026-05-27, the standalone atoms removed 2026-05-30). Activates the PBR-on-3D-mesh path (`render_3d_mesh` material=pbr, `heightmap_to_normal` WorldYUp, `bake_equirect_envmap`, `camera_orbit.pos_xyz`) — reusable for any perspective-correct PBR generator. |
| MriVolume | volumetric scrubbing: `image_folder` ×3 → `mux_texture` → `sharpen` → `smoothstep_texture` → `invert` |
| ParticleText | FluidSim2D base + text-force branch (`render_text → gaussian_blur H+V → gradient_central_diff → rotate_vec2_by_angle → gain → mix(Add) into the force chain`). The glyphs are baked into the force field as a perpendicular-curl flow, particles continuously stream along the text shape instead of being seeded at it |
| NestedCubes | instanced mesh with cycled poses: `trigger_gate` → `scalar_array_accumulator` → `cycle_table_row` → `mux_array` → `nested_cubes_geometry` |
| OilyFluid | screen-space fluid + atomized PBR: `feedback` ×2 + gradient atoms + `texture_advect` + `simplex_field_2d` → `heightmap_to_normal` → `lambert_directional` + `matcap_two_tone` + `fresnel_rim` + `blinn_specular` summed via `mix` |
| Plasma | open family on the introspected escape hatch: `clip_trigger_cycle` + `mux_scalar` → `wgsl_compute` (8 plasma variants via `switch`) — decoupled from the deleted `plasma_pattern_2d` enum primitive |
| StarField | fully atomized: `system.generator_input.time` → `math` ×3 (drift_t → offset_x/y) → `voronoi_2d` (per-cell distance + cell_hash on A) → (`scale_offset_texture` invert + `power_texture` spike) || (`channel_mix` A→R cell_hash → `smoothstep_texture` density mask + `scale_offset` ×2 freq/phase tables) → `mix Multiply` core × mask → `trig_texture` (per-pixel sin via `freq_tex`/`phase_tex` shadows) → `scale_offset` to [0,1] → `mix Multiply` apply twinkle → `scale_offset` brightness. Single-layer (cinematic 4-layer parallax dropped; revivable by duplicating the inner chain and `mix Add`-summing). Per-star unique twinkle preserved via the trig_texture texture-shadow extension. Activates `voronoi_2d` cell_hash on A + `channel_mix` GPU shader (was no-op stub) + `trig_texture.freq_tex`/`phase_tex` shadows. |
| Tesseract | 4D wireframe: `generate_tesseract_vertices` → `rotate_4d` → `project_4d` → `render_lines` |
| Text | single-primitive wrap of the CoreText glyph rasterizer: `node.render_text` |
| TrivialPassthrough | smoke test: `uv_field` |
| WireframeZoo | 3D wireframe (atom-decomposed): `clip_trigger_cycle` + `value` → `mux_scalar` → (`polytope_vertices` + `polytope_edges`) → `rotate_3d` → `project_3d` → `render_lines` |

### 6.2 Rust-defined

Empty. The migration completed in May 2026 — see [GENERATOR_DECOMPOSITION_PLAN.md](GENERATOR_DECOMPOSITION_PLAN.md) for the per-generator history.

---

## 7. Keeping this catalog honest

- After adding a new primitive: add a row to §3 under the right family and bump nothing else; the AI agent reads §3 to know what's available.
- After adding a new preset: add a row to §5 or §6.1 with the topology shape; downstream readers learn the analogue from this entry.
- After deleting a primitive: remove the row; don't leave it as "deprecated."
- Validate by running `cargo run -p manifold-renderer --bin check-presets` (loads + compiles every preset, sub-second, no GPU); a green run means every primitive referenced by every preset is registered.
