# MANIFOLD — Capability Roadmap: Graph, 3D, Video IO, Performance Clocks

A forward-looking map of what the node/graph and render system is missing measured against Notch, Blender, and TouchDesigner — and the strategic direction that follows. This is a roadmap, not a build plan: there are no tickets here, and the sizes are honest estimates, not commitments. Captured from a design brainstorm on 2026-06-17; revise as decisions firm up.

The through-line is **size**. The gaps are not equal, and the most common mistake would be treating a small engine as if it were an atom. Each item below is tagged Small (atom-sized — a new primitive or two), Medium (a real subsystem), or Large (a small engine). Every item names what it *is* and what it lets you do *on stage* — the second half is the point.

---

## 0. Where we stand

Pixel-side, the graph is **TouchDesigner-TOP complete and then some**: colour/tone, blur, the `coordinate-field → remap → mix` warp family, compositing, masks, feedback — no meaningful holes. On two axes we are *past* stock TD and Notch: the CV/AI stack (depth, person-segment, blob+track+1€, optical flow) and the particle/fluid sims (2D + 3D + camera-projected).

The gaps are not in pixels. They are in **geometry (SOP-side), 3D shading, video IO, and the performance clocks**. Audio-in is a deliberate non-gap (see §1).

---

## 1. Decisions already made (the constraints)

These are settled directions. Pin them before building anything in the later sections.

- **Audio stays first-class on the perform UI — never graph nodes.** No audio spectrum / FFT / band / onset primitives. The moment audio is just another graph input, Manifold concedes it's a TD patcher with a timeline bolted on. Audio reactivity routes through the perform-surface modulation path (param bindings), see [AUDIO_MODULATION_DESIGN.md](AUDIO_MODULATION_DESIGN.md). Memory: `audio-stays-on-perform-surface`.
- **Output mapping is deferred to Resolume.** Manifold is the source-and-processing instrument; Resolume does projection mapping / warping / slicing. This only works *because* video-out (§3) is how we hand Resolume the image — the two decisions reinforce each other. Don't build mapping.
- **Video IO becomes first-class** (§3).
- **Timecode locks the score, not the render** (§5). Manifold is not a video server. Memory: `timecode-locks-score-not-render`.
- **Cue-list is a subset of the planned Session mode** (§5) — build Session, not a standalone cue list.

---

## 2. Near-term — atom-sized, high payoff (Small)

- **Curl noise field.** Divergence-free organic flow (smoke, mist, drifting flocks) for particles and image warps, without a fluid sim — by construction it has no sources/sinks, so particles circulate instead of clumping. *We nearly have it already*: `simplex_field_2d → gradient_central_diff → rotate_vec2_90` is curl noise; the 3D curl already lives inside `curl_slope_force_3d`. This is packaging (one dispatch, animated potential, octaves), not invention. Stage: dial eddy size / churn speed live; same field drives particles *and* liquid image distortion.
- **3D primitive meshes.** Sphere (UV + ico), cylinder, cone, circle/disc, torus — as actual meshes. Today we have five Platonic solids and a grid and nothing else. Cheap, unlocks half of all 3D work. Stage: the starting block for every lit-shape-on-the-beat look.
- **Deferred shading atoms — SSAO / SSR / relight.** The G-buffer (`world_pos` / `world_normal`) is *already emitted* by `render_3d_mesh` for exactly this; the consumer atoms don't exist yet. SSAO grounds objects (biggest "rendered not flat" win), SSR gives glossy reflections, screen-space relight lets you move/recolour lights without re-rendering geometry. Stage: a light sweep across the object on the drop, driven by a macro, for the cost of one screen-space pass. NB: the textbook "deferred = many lights cheaply" reason barely applies to us — the prize is the screen-space FX, not scalability.
- **Depth-composite atom.** Merge two separately-rendered meshes by comparing their Z (near wins). This is the *cheap path to a multi-object scene* without a scene-graph rewrite — it's what first makes the word "scene" mean anything, at atom granularity. Uses the `world_pos` output we already emit.
- **Spot lights.** A cone with falloff. We have Sun and Point. Literally a stage light — a beam raking across the object, aimed and coloured on the beat.
- **N-frame delay / time-displacement.** Today only single-frame feedback exists (`node.feedback`, `array_feedback`). A delay line of N frames, or per-pixel time offset from a map, gives trails, echo, time-smear, "the image from 30 frames ago" — all currently impossible.
- **Multi-camera + cut-on-trigger.** Two or three `camera_orbit` instances and a mux switched on a clip trigger. Turns the camera into an instrument — cut between angles on the downbeat like a live video director.

---

## 3. Video IO (Small → Medium)

Two mechanisms, two directions. Syphon-first because it's almost free and it's the Resolume handshake.

**Two mechanisms, not three:** Syphon (macOS) and Spout (Windows) are the *same role* — local GPU-texture share; Spout arrives for free with the eventual Windows/Vulkan backend, not as separate work. NDI is the *network* one (cross-platform, heavier). So: **local GPU-share (Syphon/Spout)** vs **network (NDI)**.

- **Input = a source atom, generator-wrapped.** A live receiver produces an image from outside the graph — that's a source. Implement as `node.syphon_in` / `node.ndi_in`, wrapped by a thin generator preset (same shape as `Text` wrapping `node.render_text`, or `image_folder`). That buys both uses: drop it as a layer's generator ("this layer *is* the camera") *and* wire it inside any effect graph (run depth/segment on a live feed, composite it into a generative scene). Async/latency is a solved pattern here — `depth_estimate_midas` is already a background worker handing the graph its latest frame. Live input doesn't touch the clock; the beat stays authoritative.
- **Output = a passthrough "send" effect, tapped in the chain.** A node you drop into a layer or master chain that *publishes the image flowing through that point* and passes it on unchanged. The key idea: **placement is content** — tap pre- or post-effects, on a layer or on the master (no special-casing; same node, different placement). Each send carries an output *name*, so N sends = N simultaneous named virtual outputs — a feed anywhere you drop a tap, richer than Resolume's one-feed-per-layer.
- **Effort ladder:** **Syphon-out is nearly free** — it shares an IOSurface-backed Metal texture, and our entire output path is already an IOSurface zero-copy triple-buffer, so a send is wrapping a surface we already hold. **NDI is a background worker** (pixel encode UYVY+alpha + network send) — same shape as the `metal_encoder` export path / FFI workers.
- **Wrinkles for the doc:** colour-space convert on both ends (incoming BT.709/limited-range/alpha vs our HDR-ish linear internal); and nail down *where the publish happens* relative to the two-thread model — the send node describes a publish in the graph, but the IOSurface handoff must occur where the texture is real (render side), not on the content thread.

---

## 4. The 3D engine (Medium → Large)

The 3D ambition — "massive scenes with dynamic lights and shadows" — is three things of very different sizes. Deferred shading (§2) is the cheap one. The rest:

- **Geometry-nodes mesh toolkit (Medium).** Extrude, subdivide, bevel, solidify, boolean, merge-by-distance, scatter-points-on-surface, plus **3D splines + curve-to-mesh** (sweep a profile along a path → tubes/ribbons — bread-and-butter VJ geometry we can't make today; our curve work is 2D-polyline only). *Do not* chase Blender's lazy-field evaluation model — `Array<T>` + named Channels already covers ~90% of it; add the *operations*, not the evaluation-model rewrite. (`for_each_n` was correctly walked back; see the graph-compiler memory.)
- **Shadow maps (Medium).** General dynamic shadows — render scene depth from each light's POV, sample in the lighting pass. We only have the bespoke `digital_plants_render` shadow today. Pairs with deferred and spot lights; scales with shadow-casting light count.
- **Volumetric fog / light shafts (Medium).** Screen-space pass over the G-buffer. This is what actually makes a scene *read* as massive — depth haze and visible beams beat polygon count. Notch leans on it hard.
- **True scene graph (Large).** Multiple objects with a transform hierarchy (parent one to another, move as a unit), rendered into a shared depth buffer. Only take this on if the depth-composite atom (§2) starts feeling like a workaround. **Importing a Blender scene and building a scene graph are the same project** — a `.glb` *is* a transform hierarchy, so asset import forces (and answers) this question.

### 4.1 The Blender pipe — asset import (Large, strategic)

Fits the whole thesis: artists author in Blender (the best free 3D tool), Manifold is the *stage it plays on* — same as Ableton playing studio-produced stems. Manifold doesn't need to *author* 3D; it needs to *perform* it.

- **Format: glTF, specifically `.glb`** (single binary, drag-one-file, carries meshes + PBR materials + node hierarchy + cameras + lights + animation). OBJ is static-geometry-only — dead end.
- **Three animations, smallest to largest:**
  - *Rigid / transform animation* (Small-ish) — keyframed TRS on nodes; sample at current time, set transforms. Most of what a VJ wants (a logo that spins, a set piece that drifts).
  - *Vertex-cache / baked animation* (Small data-path, sneaky-good) — bake a gnarly Blender sim (cloth, fracture, fluid) to per-frame vertex positions, play back by swapping vertex buffers. No rig eval. Huge visual richness for almost no engine — an artist does something impossible-to-simulate-live and we just stream the frames.
  - *Skeletal / skinned animation* (Large) — bones deforming a mesh by skin weights; skeleton pose eval + GPU skinning. The cathedral; do it last.
- **The feature nobody else can have — the beat-retimed playhead.** An imported animation arrives with its own clock (seconds @ authored FPS). Drive its playhead with *beat atoms* instead — a `beat_ramp` scrubs progress, a trigger freezes it, loop a sub-range on the bar, scrub it backwards. The animation becomes a **beat-addressable parameter** (like `image_folder`'s position, but for a rigged mesh). A character that hits its pose *on the downbeat because you're scrubbing it* — that's the version worth building, and it's only possible because we're beat-native.
- **Artist-workflow truth to document day one:** glTF only carries the *baked* material — Principled BSDF maps onto `pbr_material` cleanly, but the full Blender shader-node tree does **not** survive export. Guidance: bake looks to textures, use Principled. See [MATERIAL_SYSTEM_DESIGN.md](MATERIAL_SYSTEM_DESIGN.md).

---

## 5. Performance clocks — scripted shows (Medium)

Manifold today is a *tempo-driven, improvisational* instrument (beats + clip triggers, synced to Ableton — the **Arrangement** half of the Ableton model, already the root). The two clocks below cover the day Manifold stops being *the* show and becomes *part of* one (opening a tour on a click + timecode; a keynote where an operator presses GO).

- **Timecode (LTC/SMPTE) — partially implemented.** The hard constraint: **timecode locks the score, not the render.** A video server scrubs a baked timeline (position → frame N); that's incompatible with us because live sims / reactivity / generators are fundamentally *not seekable* (you could only scrub them by pre-baking to video — which *is* becoming a video server). Instead TC locks the **arrangement** (section, active clips, automation positions) while forward-flowing real time still drives **generation**. At normal 1× forward playback there's zero conflict. Only hard jumps are hard, and the honest answer is re-seed/reset stateful sims + snap automation — never fake a scrub. Architecturally contained: TC is another source feeding `sync_clips_to_time`, the existing sole time authority. Memory: `timecode-locks-score-not-render`.
- **Session mode (the bigger want).** Mirror Ableton's **Session** view — the nonlinear clip grid for improvisation, beside the existing Arrangement/timeline. A **cue list** (linear list of states stepped with one GO button) is the *1D degenerate case* of Session, so build Session and the cue list falls out for free — don't build it separately.
- **The symmetry that ties it together:** the two Ableton views map straight onto the two clocks. **Arrangement + timecode = the scripted, locked, hits-the-same-every-night side. Session + GO/beat = the free, improvised, breathes-with-the-room side.** These aren't unrelated features — they complete the Ableton model the whole product is positioned on (see `positioning-ableton-m4l`).

---

## 6. Strategic forks to settle first

Two decisions gate the big 3D work because they change what the 3D layer *is*:

1. **Procedural-only, or asset-importing?** A procedural-geometry playground vs a scene compositor that also ingests authored Blender assets. Shapes everything downstream (§4.1).
2. **Depth-composite atom, or full scene graph?** Cheap-first (§2) vs architectural (§4). The atom buys most of the "multiple objects occlude correctly" win; the scene graph is for parenting/hierarchy and is forced by asset import anyway.

---

## 7. The operational layer (do not forget)

Not a feature category — the spine. This is live-performance code; *a timing bug becomes the show.* We have never designed the **failure behaviour**: what happens when a generator fails to load, an NDI source drops, or a WGSL kernel throws mid-set? (Content-thread panics are a known surface — "command channel disconnected" is one.) A deliberate **show-safe failure policy** — per-layer fallback / hold-last-frame, a hardware panic/blackout that a broken graph can't block — belongs on the same page as the features, arguably above some of them.

---

## Suggested ordering (one engineer's opinion, not a commitment)

Cheapest-high-value first: **curl noise** and **3D primitive meshes**, then the **depth-composite atom** (makes "scene" mean something). **Syphon IO** in parallel (nearly free, and the Resolume handshake). Then the "dynamic lights with shadows" cluster — **shadow maps + spot lights** together — and **volumetric fog** for atmosphere, with **deferred AO/SSR** alongside. **Asset import** is the strategic fork to settle before the 3D engine proper. **Session mode** and **timecode** track the perform-side roadmap independently. And design the **failure policy** before the first show that depends on any of it.
