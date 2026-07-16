# Visual Pieces — Graph & Node Drafts

<!-- index: Draft node-graph designs for the 2026-07-08 visual-brainstorm pieces — per-piece graph structure, groups, new atoms, and card surfaces, tiered by how real their vocabulary is. Peter's pick-list; drafts, not builds. -->

**Status:** DRAFT pick-list, authored 2026-07-08 (Fable) under Peter's mandate to spec every piece from the brainstorm session so future build sessions need no new design pass. Nothing here is built. Grounded in a full §2.5 audit of the shipped registry at `048285b9` (primitive survey + preset schema read this session); future-wave vocabulary is pinned to the approved design docs where committed and **flagged PROPOSED** where not.

**How to read a piece:** *Intent* is the stage sentence. *Audit* says what ships today, what gets extended, what is genuinely new. *Graph* gives the top-level groups (GROUPING_GRAPHS.md discipline: spine visible, 6–12 boxes, control plumbing gathered). *New atoms* carry full port/param signatures and the one-dispatch statement each must satisfy. *Card* is the performer surface — ≤12 outer params, prime modulation targets marked **(mod)**. *Verify* names the gate.

**Tiers.** **A** — buildable now against the shipped registry. **B** — rides vocabulary committed in approved designs (GAUSSIAN_SPLATS_DESIGN.md §3, BOX3D_PHYSICS_DESIGN.md §2–§3). **C** — rides waves whose atom vocabulary is not yet committed (XPBD sims, multi-display, Realtime-3D P2 shadows); atom names there are PROPOSED and the wave design may rename them.

## Pick-list summary

| # | Piece | Kind | Tier | New atoms | Size | Register |
|---|---|---|---|---|---|---|
| L1 | Log curve on `reinhard_tone_map` | extension | A | 0 (one enum arm) | XS | — |
| L2 | ~~`node.palette`~~ KILLED 2026-07-16 — pieces use `node.gradient`; see section | — | — | — | — | — |
| A1 | Murmuration | generator | A | 2 (+1 pending audit: bird-mesh heading) | M | both |
| A2 | Cymatics | generator | A | 0 | S | quiet |
| A3 | Reaction–Diffusion | generator | A | 0 (wgsl_compute) | S | quiet |
| A4 | Caustics | generator | A | 1 (tiny) | S | quiet |
| A5 | Film Master Chain | effect | A | 0 | S | both |
| A6 | Print Misregistration | effect | A | 0 (+1 extension) | M | both |
| A7 | Pressure | effect | A | 0 | S | drop |
| A8 | Mask → Explode | effect | A | 1 (tiny) | S | drop |
| A9 | Slit-Scan | effect | A | 2 (infra) | M | both |
| A10 | Growth (grow-then-explode) | generator | A | 1 (large CPU) | L | both |
| A11 | Lightning (single canvas) | generator | A | 1 (CPU) | S | drop |
| A12 | What Survives | effect | A | 0 (v0) | M | quiet |
| A13 | Glossolalia (+ One Take / lyric variants) | generator | A | 1 (large CPU) | L | quiet |
| A14 | Fork | routing pattern | A | 0 | XS | both |
| A15 | Frozen | generator | A | 0 | M | quiet |
| A16 | Schematic | generator | A | 1 (tiny) | M | both |
| A17 | I Will Not Remember This | text cues | A | 0 | XS | quiet |
| B1 | Monolith Collapse | composition | B | 0 beyond waves | M | set-piece |
| B2 | Video-Textured Rubble | composition | B | 1 PROPOSED + 2 extensions | M | drop |
| B3 | Physics-as-Clip conventions | conventions | B | 0 | XS | — |
| B4 | Render Fader | composition | B | 0 beyond waves | S | both |
| B5 | Splats Through Slit-Scan | composition | B | 0 beyond A9 | XS | quiet |
| B6 | Cel Screen-Print | composition | B (core is A) | 0 beyond A6 | S | both |
| C1 | Towers as Elements | composition | C | XPBD wave | L | both |
| C2 | Lightning Between Towers | composition | C | 0 beyond A11 | S | drop |
| C3 | Wind Made Visible | generator | C (approx today) | 0 today | M | quiet |
| C4 | Shadow as Subject / Gallery After Dark | composition | C | Realtime-3D P2 | M | quiet |

## Conventions every draft assumes

- **Names** follow DECOMPOSING §6.6: plain language, no math jargon in type ids, node `title`s on every ambiguous node, every `wgsl_compute` titled.
- **Every numeric scalar param on a new atom ships port-shadowed** (DECOMPOSING §6.2 authoring rule). Enum/Bool mode selectors are the only exceptions.
- **No dead-state params** (§7): every card slider does something in every reachable state.
- **Static rescaling lives on the binding** (`scale`/`offset` on `BindingDef`), never as a `math` node (GROUPING §4).
- **Array producers declare `array_output_capacity`**; trigger→selection goes through `ClipTriggerCycle`; state lives in `extra_fields`; zero per-frame allocations in `run()`.
- **Texture currency is f16**; density pipelines are `u32` fixed-point accumulators resolved by `node.resolve_scatter`.
- **Gates:** `cargo run -p manifold-renderer --bin check-presets` (every JSON edit) → one-frame-execute test (`bundled_generator_presets` / `bundled_presets`) → headless PNG render + actually look at it. Math-heavy new atoms get value-level `gpu_tests`; pure-look shading atoms skip heavy parity (per `visual-effects-skip-gpu-parity`).
- **Determinism:** every stochastic atom takes a `seed` param and derives per-frame randomness from `(seed, frame/beat)` — never wall-clock — so exports reproduce.
- **Executor-readiness (added 2026-07-08, Peter's requirement):** these drafts will be built by Opus/Sonnet sessions with no Fable in the loop, so **no acceptance criterion may be a pure taste judgment.** Every "looks right" has a scripted numeric proxy stated in the piece's Verify step (convergence deltas, autocorrelation, topology counts, IoU against a reference map) — the executor builds to the proxy, and Peter's look-pass (headless PNG or live) is a separate, explicitly-owed L4 gate, never the executor's job. Where a piece's quality genuinely lives in tuning (palettes, default card values), the draft says so and marks the tuning Peter-owed.

---

## L1. Log curve on `node.reinhard_tone_map` (lever)

**Intent.** The flame-fractal response for every density pipeline: log reveals structure across the faint-to-hot range that Reinhard compresses away. Unblocks A1/A2/A4/A10/A11's render quality with one enum arm.

**Audit.** `reinhard_tone_map` ships two curves (Extended — matches FluidSim bit-for-bit — and Simple). Extend, don't add a node (§6.2): existing curves must stay bit-identical.

**Change.** `curve` enum gains `Log`: `out = log2(1 + x·exposure) / log2(1 + white·exposure)`, params `exposure` (port-shadowed, default 8.0) and `white` (default 64.0) — both already-present or added as port-shadowed floats that the existing curves ignore-free (they reuse `exposure` as pre-gain so no dead param: Extended/Simple apply it as linear pre-multiply, today's behaviour at 1.0).

**Verify.** `gpu_tests` value-level: Log at known (x, exposure, white) triples; Extended/Simple regression rows unchanged bit-for-bit.

## L2. ~~`node.palette` — curated identity LUTs~~ (KILLED 2026-07-16)

**Killed by Peter, two strikes.** (1) The original compiled-enum shape locks palette
authoring behind the Rust compiler — users must be able to make their own colours.
(2) The data-file revision (palette JSONs in a scanned dir) solves nothing that isn't
the *general* reusable-component problem, which the component-library direction
(post-release board) already owns: a saved `node.gradient` with curated stops becomes
a library component like any other, alongside film-grain blocks and force chains.
A palette-specific mechanism is a point solution — don't re-propose.

**What pieces do instead:** every "→ `color_lut(palette)`" in this doc reads as
"→ `color_lut(node.gradient)`" with curated stops authored per preset (Table param,
editable in the graph editor today — `EditGraphNodeTableCell`). A "Palette" card =
whatever binding the piece wants on the gradient/grade path. Re-tuning a colour
across many presets is a find-and-replace until the component library lands; that
pain arriving is the signal to prioritise the component library, not a palette node.

---

## A1. Murmuration (generator)

**Intent.** Thousands of starlings as accumulated ink density — onsets scatter the flock, silence regroups it. The cheap crowd-piece that later upgrades into an instrument.

**Audit.** *Reuse:* `spawn_particles`, `array_feedback`, force chain (forces add in-place to a shared force buffer: `turbulence`, `add_burst`), `move_particles`, `draw_particles` → `resolve_scatter` → `reinhard_tone_map(Log)` → `color_lut`, `feedback` + `compose` for trails, `beat_gate`/`envelope_follower_ar` for onset bursts. *New:* neighbor binning + the flock force — two atoms, deliberately split so the bins are reusable vocabulary (SPH, future proximity effects).

**Graph** (top level, spine visible):

```
Inputs → Spawn Birds → [Flock State: array_feedback] → Neighbors → Forces → Integrate ↩
                                            Flock State → Render Density → Trails → Grade → Output
```

- **Spawn Birds** — `spawn_particles` (count card via binding, grid/disc init), reset on trigger.
- **Flock State** — the one `array_feedback`, top-level (the loop must read at a glance).
- **Neighbors** — `node.neighbor_bins` (below).
- **Forces** — `node.flock_force` (below) → `turbulence` (adds wander) → `add_burst` (adds panic; `amount` wired from `envelope_follower_ar(trigger)` — the kick scatters the flock, decay regroups it).
- **Integrate** — `move_particles` (speed card).
- **Render Density** — `draw_particles` → `resolve_scatter` → slight `gaussian_blur`.
- **Trails** — `feedback` × decay `gain` → `compose(Max)` with fresh density (ink persists, never blows out).
- **Grade** — `reinhard_tone_map(Log)` → `color_lut(node.gradient)`.

**Bird-mesh render mode (added 2026-07-16, Peter's differentiation pass).** The
density/ink render is one costume among three particle pieces (A1/A2/A4); to keep
Murmuration from reading as "same dots, different physics," the preset ships a second
render group: a low-poly bird glTF (**CC0 source only** — Kenney/Quaternius class,
never a ripped game asset; licensing matters at release) through `gltf_mesh_source` →
`render_instanced_3d_mesh`, one instance per particle. The missing wire is
particles→instance-transforms **with heading**: a bird must point along its velocity
and bank into turns or the flock reads as confetti. That is one small stateless atom
(orientation basis from velocity + position → instance transform array) — **§2.5
audit at build time before committing to it**; the instancing vocabulary
(`generate_instance_transforms`, `lerp_instance_fields`, per-copy noise) may already
carry a usable piece. Density mode stays for distance/ink looks; a Look enum on the
card selects the render group (both wired, `mux_texture` at the end — no fused
monolith, it's graph routing).

**New atoms.**

| | `node.neighbor_bins` | `node.flock_force` |
|---|---|---|
| class | one dispatch (atomic linked-list binning) | one dispatch (3×3 bin gather per particle) |
| inputs | `particles: Array(Particle)` | `particles: Array(Particle)` · `bins: Channels[HEAD: U32, NEXT: U32]` |
| outputs | `bins: Channels[HEAD: U32, NEXT: U32]` | `force: Array(vec2<f32>)` (adds in-place, chainable like the shipped force atoms) |
| params | `grid_res: Int` (default 64) | all port-shadowed: `cohesion`, `alignment`, `separation`, `sight_radius`, `max_force`, `home_x`, `home_y`, `home_pull` |
| state | none (buffers rewritten per frame) | none |

`home_pull` doubles as the canvas-bounds policy (soft pull toward home point) and the musical "regroup" control — no separate containment atom needed, no dead param.

**Card** (10): Birds (1k–200k) · Cohesion · Alignment · Separation · Sight · Speed · **Scatter (mod: onset/kick envelope)** · **Regroup (mod: inverse energy)** · Trail · Palette.

**Verify.** `gpu_tests` on `flock_force` (3-particle hand-computed cohesion/separation cases); headless PNG sequence — flock must read as murmuration, not sprite cloud, before shipping.

## A2. Cymatics (generator)

**Intent.** Chladni plate: sand settles onto the nodal lines of a standing wave; pitch changes physically rearrange the sand. Sound shaping matter — the thesis statement, for quiet sections.

**Audit.** **Zero new atoms.** The plate field is closed-form and composes from shipped per-pixel atoms; the sand is the shipped particle stack; the one subtle piece — sand jiggling *except* on nodal lines — falls out of `anti_clump_particles`' existing `strength_modulator` texture input.

**Graph:**

```
Inputs → Plate Field → Sand Forces → [Sand State: array_feedback] → Integrate ↩
                              Sand State → Render Sand → Grade → Output
```

- **Plate Field** — `uv_field` → `sin_term`(x-axis, `freq` ← Mode X card, binding `scale: π`) and `sin_term`(y-axis, `freq` ← Mode Y) → `compose(Multiply)`; the swapped pair likewise; `compose(Add|Difference)` (Symmetry card toggles the ± plate family) → `node.absolute_value`. Result: bright = loud plate, black = nodal lines.
- **Sand Forces** — `scale_offset_image`(×−1) → `edge_slope` (gradient of −|field| — force points *toward* nodes) → `sample_image_at_particles` → force buffer; `anti_clump_particles` adds Brownian jiggle **with the |field| texture wired to `strength_modulator`** — sand vibrates violently off-node, freezes on the lines. That's the physical tell that sells it.
- **Integrate / Render / Grade** — `move_particles` → `draw_particles` → `resolve_scatter` → `reinhard_tone_map(Log)` → `color_lut(palette)` → `compose` over a near-black plate (`linear_gradient` at low gain).

**Card** (9): **Mode X (1–12, whole)** · **Mode Y (1–12, whole)** (both prime mod targets — bind to pitch/band via the audio-mod system) · Symmetry (toggle) · Sand (count) · Settle (force gain — **mod: inverse energy**, drops shake the plate) · Jiggle · Trail · Palette · Cycle Modes (toggle: `clip_trigger_index` pair walks a curated (n,m) table on clip triggers).

**Verify.** Computable oracle: nodal lines of the rendered field must match the analytic zeros of `sin(nπx)sin(mπy) ± sin(mπx)sin(nπy)` — three (n,m) pairs, script-checked on the PNG. Then look: sand must *settle*, not orbit.

## A3. Reaction–Diffusion (generator)

**Intent.** Gray-Scott growth — coral, fingerprints, labyrinths — seeded by kicks, morphing between regimes as a performable move. The organic-growth texture family.

**Audit.** Zero new atoms. The RD update is DECOMPOSING §5's *named example* of a legitimate `wgsl_compute` case (domain-specific coupled kernel, format-sensitive feedback); the ping-pong is `temporal`; seeding, display, palette all shipped.

**Graph:**

```
Inputs → Seed → [Field Memory: temporal] → React ×4 → Field Memory ↩
                                   React → Develop → Grade → Output
```

- **Seed** — `circle_mask` (Inject X/Y cards) × `envelope_follower_ar(trigger)` → `compose(Max)` into the loop (a kick stamps fresh V-chemical into the dish).
- **Field Memory** — one `temporal` (rg16float; U in R, V in G), top-level spine pivot.
- **React ×4** — four chained `wgsl_compute` nodes, all **titled** (`React 1`…`React 4`, per GROUPING §7), same JSON-editable Gray-Scott kernel: 5-point Laplacian, `feed`/`kill`/`diff_u`/`diff_v`/`dt` uniforms. Four substeps/frame is the speed/stability sweet spot; Speed card scales `dt`.
- **Develop** — read V → `levels` → optional `edge_slope` rim → `compose(Screen)`.
- **Grade** — `color_lut(palette)`.

**Card** (9): **Feed** · **Kill** (the regime plane — prime mod pair; a slow LFO across them tours spots→stripes→waves live) · Diffusion · Speed · Inject X · Inject Y · **Inject (mod: kick envelope)** · Contrast · Palette. Optional: Regime cycle via `cycle_table_row` of curated (feed,kill) pairs on clip trigger.

**Verify.** check-presets; PNG time-series at three canonical regimes (spots f=.035/k=.065, stripes f=.045/k=.060, waves f=.014/k=.045) — patterns must match the known Gray-Scott morphology.

## A4. Caustics (generator)

**Intent.** Light through water onto a floor — the universally-liked one. Doubles as a light layer over video (effect-side variant is the same graph with `compose(Add)` onto `system.source`).

**Audit.** *Reuse:* height field (`simplex_field_2d` with `z = time·speed`), `edge_slope`, per-frame photon grid (`seed_particles` re-emits fresh each frame when no feedback loop closes over it), `sample_image_at_particles`, `draw_particles`/`resolve_scatter` (forward scatter is what *concentrates* light — a gather/remap can't brighten fold lines), Log tonemap (L1), `pack_channels` for dispersion. *New:* one tiny stateless atom — particles need `position += sampled offset`, which no shipped atom does without integrating velocity state.

**New atom.**

| | `node.offset_particles` |
|---|---|
| class | one pointwise dispatch, stateless |
| inputs | `particles: Array(Particle)` · `offset: Array(vec2<f32>)` |
| outputs | `particles: Array(Particle)` (position.xy += offset · amount) |
| params | `amount: Float` (port-shadowed) |

Reusable anywhere a rest shape takes a per-frame displacement without velocity state (cymatics variant, dust-on-glass, Glossolalia jitter).

**Water-surface realism (added 2026-07-16).** Single-octave simplex reads as blobby,
isotropic ripples and the caustic network inherits whatever the surface is — real
water has directional wave character and the network gets its fine cellular structure
from it. Build the height field from 2–3 octaves of animated noise (or noise +
a slow directional sine component) — stays in JSON, no new atoms; the octave mix is
an authoring decision judged on the PNG.

**Graph:** **Water** (`simplex_field_2d(z=time)` → `edge_slope`) → **Photons** (`seed_particles` grid → `sample_image_at_particles(gradient)` → `offset_particles(amount = Depth)`) → **Focus** (`draw_particles` → `resolve_scatter` → small `gaussian_blur` → `reinhard_tone_map(Log)`) → **Grade** (`colorize` water tint or `color_lut`) → `compose` over `linear_gradient` deep-water ramp. **Dispersion** (optional group): three `offset_particles` at amount ×0.98/1.0/1.02 → three resolves → `pack_channels` → chromatic fringing on the fold lines.

**Card** (8): **Depth (mod: sub-bass — water gets violent with the low end)** · Scale · Speed · Sharpness (blur⁻¹ + log exposure, one card fanned to both bindings) · Dispersion · Sun Angle (constant added to offset via `scale_offset_image` on the gradient) · Tint/Palette · Photons.

**Verify.** PNG at Depth 0 (uniform field — no pattern) and Depth mid (folded caustic network); the fold lines must brighten, not blur — that's the scatter-vs-gather check.

## A5. Film Master Chain (effect)

**Intent.** The mastering bus: halation, grain, gate weave, vignette in linear HDR. Not a look — it makes every other preset read as expensive.

**Audit.** **Zero new atoms.** `levels`, `gaussian_blur`, `colorize`, `compose(Screen)`, `film_grain`, `lfo(S&H)`, `smoothing`, `affine_transform`, `vignette`, optional `tone_map(AgX)`. Pure preset authoring.

**Graph:** source → **Gate Weave** (two slow `lfo`(S&H) → `smoothing` → `affine_transform` translate ±3 px, rotate ±0.1°) → **Halation** (`levels` threshold ≥1.0 highlights → `gaussian_blur` wide → `colorize` warm red-orange → `compose(Screen)` — the red bloom hugs highlights because only HDR energy passes the threshold; must sit *before* any tonemap, which the chain's linear f16 currency already guarantees) → **Grain** (`film_grain`) → **Frame** (`vignette`) → out.

**Card** (6): Halation · Halation Size · Grain · Weave · Vignette · Warmth. All authoring-leaning; the chain is meant to sit still while other things move.

**Verify.** check-presets + A/B PNG over a bright-highlight fixture frame; halation must hug only >1.0 energy (feed an SDR-max frame — no bloom = correct).

## A6. Print Misregistration (effect)

**Intent.** CMYK halftone plates that drift out of register and snap back on the beat — the moving screen-print. The "designed, not generated" aesthetic lane, massively legible at LED distance.

**Audit.** *Reuse:* `channel_mix` (isolate a channel to all-RGB), `invert` (RGB→CMY), `saturation(0)` + `invert` (K plate from luma), `node.dither` + `node.dither_pattern` (six ordered/halftone threshold algorithms ship), `affine_transform` (plate offset), `compose(Multiply)` (subtractive recombine), `noise` (paper), `lfo`+`smoothing` (drift), `beat_gate`→`envelope_follower_ar` (snap-back). *Extension (one):* `dither_pattern` gains an `angle: Float` (port-shadowed) param rotating its screen — classic print screens sit at per-ink angles (C 15°, M 75°, Y 0°, K 45°) and the moiré between rotated screens is the look. Additive, default 0 = today's output.

**Graph:**

```
source → Plates (×4) → Screens (×4) → Register (×4) → Press → Paper → Output
                                     Drift & Snap ──↗ (offsets)
```

- **Plates** — C = `invert(channel_mix[R])`, M = `invert(channel_mix[G])`, Y = `invert(channel_mix[B])`, K = `invert(saturation(0))`.
- **Screens** — per plate: `dither_pattern(angle = 15/75/0/45 + Angle Jitter)` → `node.dither` (Dot Size card → pattern scale binding).
- **Register** — per plate `affine_transform`; offsets = per-plate `lfo`(free, incommensurate rates) × Register card × (1 − snap envelope). `envelope_follower_ar(beat_gate)` pulls all four plates home on the beat — misalign in the space between beats, snap to register on the hit.
- **Press** — `compose(Multiply)` chain C·M·Y·K over white.
- **Paper** — `noise(Random, low amount)` `compose(Multiply)` — paper tooth.

**Card** (7): Dot Size · **Register (mod: energy — the mix drifts apart as the track leans in)** · Drift Speed · Snap (toggle) · Ink (levels gain) · Paper · Angle Jitter.

**Verify.** PNG at Register 0 (plates must recombine to ≈source through halftone) and Register high (visible CMYK fringing); `gpu_tests` row for `dither_pattern` angle=0 regression (bit-identical to today).

## A7. Pressure (effect)

**Intent.** The sub-bass doesn't show a hit — the tower takes one. Radial bulge with chromatic fringing at the swell, driven by the Low band. The towers-as-real-glass move at 140 BPM.

**Audit.** **Zero new atoms.** `radial_offset_field` (radial displacement field generator) → the shipped coordinate-field → `remap` pattern; `chromatic_displace` reads the same field as its velocity (RG); `smoothing` gives the hit elasticity.

**Graph:** **Bulge** (`radial_offset_field` radial mode; Center X/Y cards; amount = Punch card through `smoothing` — the one-pole makes hits *ring* instead of stepping) → **Displace** (`uv_field` + field → `remap(Absolute)`) → **Fringe** (`chromatic_displace`, velocity = same field, amount = Fringe card) → out.

**Card** (5): **Punch (mod: Low band send — this card IS the effect)** · Center X · Center Y · Fringe · Elasticity (smoothing time-constant).

**Verify.** check-presets; PNG pair at Punch 0/max — bulge must displace outward with fringe on the gradient, no wrap artifacts at frame edge (out-of-bounds sampling policy check).

## A8. Mask → Explode (effect)

**Intent.** A named thing in the footage lifts out and bursts to particles on cue. Ships today against `person_mask`; `segment_anything` (ML wave) later upgrades "person" to "anything you can name" with zero graph changes.

**Audit.** *Reuse:* `person_mask`, `seed_particles_from_texture` (exact-placement seeding from a mask — the compact+place two-pass already shipped), `add_burst`, `turbulence`, `move_particles`, `array_feedback`, density render stack, `masked_mix` (hole-punch the subject from the source), `trigger_gate`. *New (tiny):* nothing pulls particles *down* — a constant-force atom.

**New atom.**

| | `node.constant_force` |
|---|---|
| class | one pointwise dispatch, stateless (adds in-place, chainable with shipped force atoms) |
| inputs | `force: Array(vec2<f32>)` |
| outputs | `force: Array(vec2<f32>)` (+= (x, y) · amount) |
| params | port-shadowed: `x` (default 0), `y` (default −1), `amount` |

Gravity, wind, updraft — one atom, reused by A1/A2 variants immediately.

**Graph:** **Subject** (`person_mask` → `seed_particles_from_texture`, re-seed gated on the Explode trigger edge) → **Forces** (`add_burst` at subject center, amount = `envelope_follower_ar(trigger)` → `turbulence` → `constant_force` gravity) → **Integrate** (`move_particles` + `array_feedback`) → **Render** (`draw_particles` → `resolve_scatter` → `reinhard_tone_map(Log)` → `color_lut`) → **Composite** (`masked_mix`: source with subject region faded to background as the burst envelope rises, then `compose(Add)` particles).

**Card** (7): **Explode (trigger — bind to a clip trigger or drop marker)** · Force · Turbulence · Gravity · Fade (trail decay) · Palette · Re-arm (toggle: re-seed while idle so the next hit always has a fresh subject).

**Verify.** Runtime check with a person fixture clip: subject must vanish *as* particles appear (same frame), no double-exposure; particles must originate on the silhouette, not the bbox.

## A9. Slit-Scan / Time Displacement (effect)

**Intent.** Per-pixel time travel: a dancer smears into ribbons, near-things stay present while far-things lag into the past. The "time as material" family — needs the one real piece of infra in Tier A.

**Audit.** *Reuse:* `Texture3D` machinery already ships (`blur_3d`, `sample_volume_2d`, `slice_volume`, 3D accumulators), `linear_gradient`/`depth_map`/`luminance`-style maps for the delay source, `mux_texture` for map select, `invert` for direction. *New:* the ring buffer and the per-pixel time sampler — both broadly reusable (echo, onion-skin, temporal feedback family).

**New atoms.**

| | `node.frame_history` | `node.time_displace` |
|---|---|---|
| class | one copy dispatch/frame, stateful | one dispatch, stateless |
| inputs | `in: Texture2D` | `history: Texture3D` · `delay: Texture2D` (R = 0..1) |
| outputs | `history: Texture3D` (ring; slice 0 = newest) | `out: Texture2D` |
| params | `frames: Int` (8–64, default 32) · `downsample: Enum` (1/2/4, default 2) | `max_delay: Float` (0..1 of buffer, port-shadowed) · `filter: Enum` (Nearest \| Blend across time) |
| state | Texture3D ring + write cursor in `extra_fields` | none |

**Memory honesty:** the ring is the cost — 32 frames at half-res 1080p f16 ≈ 66 MB, at quarter-res ≈ 17 MB. Defaults ship quarter-res/32; the atom logs its allocation as a report line, and `frames × resolution` is clamped with the clamp logged (no silent cap).

**Graph:** source → **History** (`frame_history`) ; **Delay Map** (`mux_texture`: `linear_gradient` vertical | horizontal | `depth_map` (near=now, far=past — the showstopper) | source `luminance`; Map card = selector; `invert` behind a Direction toggle) → **Displace** (`time_displace`) → out.

**Card** (5): **Delay (mod-able — riding it live *pumps* time)** · Map (enum) · Direction (toggle) · Smear (time filter) · Resolution (authoring, whole).

**Verify.** `gpu_tests` on `time_displace` (known 4-frame history, step delay map → exact slice selection); then the look: waving-hand fixture through the vertical map must produce the classic ribbon.

## A10. Growth — grow, then explode (generator)

**Intent.** The quiet section grows a branching structure up the portrait tower, branch by committed branch; the drop detonates it into the 3D particle stack. One asset, two energies, one preset.

**Audit.** *Reuse:* the entire downstream: `rotate_3d` → `project_3d` → `render_lines` (glow via `feedback` + `compose`); **`spawn_from_mesh` already shipped** (vertices mode), `add_burst_3d`/`swirl_force_3d`/`diffuse` forces, `move_particles_3d`, `draw_particles_camera` (fused camera projection + scatter), `resolve_scatter`, Log tonemap, `mux_texture` crossfade on `trigger_gate`. *New (the piece's one real cost):* the growth engine.

**New atom.**

| | `node.grow_branches` |
|---|---|
| class | one CPU operation/frame (space colonization step), stateful |
| inputs | port-shadowed scalars: `growth` (0..1 — the master reveal), `reset: trigger` |
| outputs | `vertices: Array(MeshVertex)` · `edges: Array(EdgePair)` · `tip_count: Scalar` |
| params | `shape: Enum` (Column \| Dome \| Sphere — Column default, portrait-native) · `attractors: Int` (density) · `step: Float` · `capture_radius` · `kill_radius` · `thickness_taper` · `seed: Int` · `max_capacity` (declares array capacity) |
| state | attractor set + grown segment list (pre-allocated to capacity) in `extra_fields`; regrown deterministically from `seed` on reset |

Space colonization is one algorithm with one job — siblings (DLA, L-systems) arrive as their own atoms later, not as modes (§6.3: don't pre-fuse a family from one member). `growth` maps monotonically onto the already-grown segment list (segments carry birth order), so scrubbing it backward is free and export-deterministic.

**Graph:**

```
Inputs → Grow → Turn (rotate_3d ← lfo yaw) → Flatten (project_3d) → Draw Branches (render_lines + glow feedback)
              ↘ Detonate (spawn_from_mesh → burst_3d → swirl → move_3d → draw_particles_camera → resolve → Log)
                          Draw Branches / Detonate → Crossfade (mux_texture ← trigger_gate) → Grade → Output
```

**Card** (9): **Grow (0..1 — bind to `beat_ramp` over 8 bars, or ride it by hand; the piece IS this fader)** · Shape · Density · Twist · **Explode (trigger)** · Burst · Glow · Palette · Seed (whole — reroll the tree).

**Honest limit:** v1 is orbit/turn, not a fly-*through* — `render_lines` draws pre-projected curves and has no camera input. The fly-through upgrade is a `render_lines`-with-`Camera` extension (or tube meshes via `make_triangles` + `render_mesh`), noted for the Realtime-3D wave; the grow/detonate arc doesn't wait for it.

**Verify.** `gpu_tests`-style CPU test on `grow_branches` (fixed seed → segment count monotone in `growth`; all segments connected; capacity respected). Executor-grade shape proxies (scripted, per the conventions note): in Column mode at growth 1.0, height/width ratio ≥ 2.5; branch-angle distribution stays within ±75° of parent direction; zero segment self-intersections; segment count within 10% of `attractors`-predicted. Peter's L4: PNG series at growth 0.25/0.5/1.0 — does it read as a plant.

## A11. Lightning (generator, single canvas)

**Intent.** A grown bolt — snare-quantized strikes with branch decay and afterglow. Nature's fast twin to A10; C2 later stretches it across the physical tower gap.

**Audit.** *Reuse:* `trigger_gate`, `envelope_follower_ar` (flash + afterglow envelopes), `render_lines` (bright core + dim branches are two draws), `feedback` decay, `gaussian_blur` bloom, `flash`, Log tonemap, palette. *New:* the bolt geometry.

**New atom.**

| | `node.lightning_bolt` |
|---|---|
| class | one CPU operation on strike (midpoint-displacement + recursive branching), stateful |
| inputs | `strike: trigger` (rising edge = new bolt) · port-shadowed: `x0, y0, x1, y1` (endpoints) |
| outputs | `core: Array(CurvePoint)` · `branches: Array(CurvePoint)` · `age: Scalar` (frames since strike) |
| params | `jag: Float` · `branch_count: Int` · `branch_decay: Float` · `detail: Int` (subdivision depth) · `seed_mode: Enum` (Reroll \| Fixed) · `max_capacity` |
| state | current bolt polylines (pre-allocated), strike age |

**Width taper (added 2026-07-16, Peter's realism pass).** Uniform line width is the
single biggest cheesy-lightning tell. The bolt carries per-vertex thickness — thick
at the trunk, hairline at branch tips, decaying with branch generation — emitted in
the `CurvePoint` stream so `render_lines` draws the taper (verify `render_lines`
honours per-point width at build time; if it doesn't, that extension lands with this
piece). The other anti-cheese defenses are already in the graph: near-white HDR core
(branches at 0.3), single-frame strike with only the afterglow decaying, wide bloom
through the Log tonemap — never saturated purple, never slow cartoon animation.

**Graph:** **Strike** (`trigger_gate` — card trigger or clip trigger) → **Bolt** (`lightning_bolt`, endpoints default top→bottom, portrait-native) → **Draw** (`render_lines` core at full intensity + `render_lines` branches at 0.3, `compose(Add)`) → **Afterglow** (`feedback` × decay, `compose(Max)`) → **Air** (`gaussian_blur` wide → `compose(Screen)` — the bloom) → **Flash** (`node.flash` ← `envelope_follower_ar(strike)`, fast decay — the whole frame kicks) → Log tonemap → palette (electric blue-white default).

**Card** (7): **Strike (mod: snare/onset — the instrument)** · Jaggedness · Branches · Afterglow · Flash · Reach (endpoint spread) · Palette.

**Verify.** CPU test: fixed seed → identical polyline twice (determinism); PNG triptych strike/+3 frames/+10 frames — core gone, afterglow decaying, no accumulation blowout.

## A12. What Survives (effect — self-portrait I)

**Intent.** Re-describe a frame through the instrument's own perception nodes and redraw it from only the description, feeding the redraw back in. Loss is constitutive; the image converges to the machine's prior. One fader: let reality back in, or let it drift. (Ancestor: Lucier, *I Am Sitting in a Room*.)

**Audit.** **Zero new atoms for v0.** `temporal` (the memory), `edge_detect`, `depth_map`, `person_mask` (describers — all lag-tolerant/async by design; between inferences the last maps persist, which *adds* to the drift character rather than fighting it), `node.gradient` (L2 killed) + `color_lut` (palette fill), `posterize` (flat confident fields), `masked_mix`, `compose`, `wet_dry`. v1 option: `node.palette_from_image` (k-means sampled palette, CPU, ~S-size) makes the palette genuinely *sampled* instead of authored — deferred until the piece proves itself.

**Graph:**

```
source ──┐
         ├→ Admit Reality (wet_dry ← Drift card) → Describe → Redraw → OUT
[Memory: temporal] ←──────────────────────────────────────────┘
```

- **Admit Reality** — `wet_dry(dry = source, wet = Memory, mix = Drift)`. Drift 0: every frame is described once from life (reads as a stylize). Drift 1: the loop eats only itself and converges to the prior. The card is the dramaturgy.
- **Generation Clock** — `beat_gate` (Cadence card: every beat / every bar) latching the admitted frame through a `mux_texture` hold. Generations are **discrete and musical**: the image sits perfectly still between re-descriptions, then re-remembers itself once per bar. Lucier's generations were discrete tape passes; per-frame looping is what reads as smear.
- **Describe** — `edge_detect` · `depth_map` · `person_mask`, all reading the latched frame.
- **Redraw** — depth → `color_lut(palette)` (tonal fill from distance) → `posterize` (Bands card — the flat, confident fields) → subject re-tinted via `masked_mix(person_mask)` → edges re-inked via `compose(Multiply)` dark strokes (Detail card). Output goes to OUT **and** into Memory.

**The re-description contract (anti-smear, load-bearing).** This piece is one graph decision away from the day-one TouchDesigner feedback loop (feedback → blur/displace/hue-drift → accumulate), and the build must hold the line that separates them: **the previous generation's pixels never reach the next generation — only the description maps do.** The Redraw group's inputs are exclusively {edges, depth, mask, palette}; there is no wire from Memory into any Redraw compositor, no geometric transform inside the loop, no continuous accumulation. The TD loop *re-processes* pixels and drifts forever; this piece *re-describes* and **converges** — detail below the describers' thresholds is irreversibly gone each generation, which is the entire meaning. If a build change makes the image smear, orbit, or color-cycle instead of settling into flat confident fields, the contract is broken regardless of how good it looks.

**Card** (6): **Drift (THE fader — quiet-section dramaturgy in one knob)** · Cadence (beat / bar / free) · Palette · Detail (edge ink) · Bands · Reset (trigger — flushes Memory to source).

**Verify.** The contract is the acceptance test: 60 s soak at Drift 1 from a face fixture — per-generation frame delta must **decrease monotonically toward ~zero** (converge), never oscillate or blow out; a scripted delta check on the PNG series, then look. Also: `temporal` state resets on export warmup (§8 bug class); no wire exists from Memory into Redraw (structural check on the JSON).

## A13. Glossolalia (generator — self-portrait II)

**Intent.** A hand writing an asemic script, stroke by committed stroke, the unchosen candidate strokes ghosted around the pen, temperature on a fader. At temperature 0 the script calcifies into loops (mode collapse as visible behaviour); too hot it dissolves into scribble; the life is the middle band. Columns write downward — portrait-native.

**Audit.** *Reuse:* `render_lines` (committed ink + ghost fan are two draws), `feedback` (the page — ink accumulates because the canvas loop holds it, *not* inside the atom), `compose(Add/Max)`, `circle_mask` + `flash` (pen glow), `envelope_follower_ar` (page-turn flush), `color_lut`. *New (the piece):* the pen.

**New atom.**

| | `node.script_pen` |
|---|---|
| class | one CPU operation/frame (advance the writing state, emit this frame's geometry), stateful |
| inputs | `beat: Scalar` (from `generator_input` — strokes commit on subdivisions; the pen writes in rhythm) · port-shadowed: `temperature` (0..2) · `rate` (strokes per beat) · `page_turn: trigger` |
| outputs | `strokes: Array(CurvePoint)` (segments committed THIS frame only) · `ghosts: Array(CurvePoint)` (top-k candidate continuations, re-emitted fresh each frame, never accumulated) · `pen_x, pen_y: Scalar` |
| params | `glyph_scale` · `columns: Int` · `direction: Enum` (Down \| Right) · `ghost_count: Int` (default 5) · `seed: Int` · `ink: Float` (stroke weight) · `max_capacity` |
| state | pen position, in-glyph stroke progress, column/line layout cursor, recent-glyph habit memory (the thing temperature 0 collapses onto), seeded RNG — all pre-allocated in `extra_fields` |

Mechanism, honestly stated: candidate strokes are sampled from a hash-derived distribution conditioned on (glyph progress, habit memory); temperature scales the distribution's sharpness. T→0 = argmax = the habit loop repeats (visible mode collapse, by construction not by simulation). Ghost opacities are the actual candidate weights. Layout (glyph advance, line breaks, column wrap) is part of "writes script" — one cohesive CPU op, §1.1-clean. Deterministic per (seed, beat), so exports reproduce.

**Graph:**

```
Inputs → Pen → Ink This Frame (render_lines) → [Page: feedback ×Ink Fade] → compose(Max) ↩
         Pen → Ghost Fan (render_lines, thin) ──→ compose(Add) over page   (never enters feedback)
         Pen → Pen Light (circle_mask @ pen_x/y × flash) → compose(Screen)
                                                  → Grade (color_lut: ink-on-paper / phosphor) → Output
```

Page turn: `page_turn` trigger → `envelope_follower_ar` inverted into the feedback gain for one beat — the page wipes and writing resumes at the top.

**Card** (9): **Temperature (THE fader — freeze it in a breakdown and watch it get stuck; slam it at a drop)** · Tempo (rate, subdivision-quantized) · Ink Fade · Ghosts (fan opacity) · Glyph Size · Columns · **Page Turn (trigger)** · Palette · Seed (whole).

**Verify.** CPU determinism test (fixed seed + beat sequence → identical stroke stream). Executor-grade proxies (scripted): at T=0, stroke-sequence autocorrelation must show a repeating motif (normalized autocorrelation peak ≥ 0.8 at some lag ≤ 32 strokes); at T=2, no autocorrelation peak ≥ 0.3 (no structure); at T=0.7, glyph-height variance within 20% (line discipline holds — it still reads as *writing*). Peter's L4: the T=0 / 0.7 / 2 PNG triptych.

### A13.1 One Take (variant — the crowd-legible cousin)

Same atom, one optional input: `target: Texture2D`. When wired (an edge map of a reference photo — `edge_detect` on any image the show supplies), the candidate-stroke distribution is conditioned on the target's unclaimed edges instead of the hash prior: the pen *draws the picture*, stroke by stroke, over ~2 minutes, ghosts and hesitation visible, and a wrong stroke is never erased — it's incorporated (no-undo is the piece, and every human who has drawn in pen reads it instantly). Temperature still governs: low = careful draftsman, high = the drawing barely holds together. **Executor proxy:** at completion, IoU between the inked strokes (rasterized) and the target edge map ≥ 0.5 at T=0.5; Peter's L4 judges whether it *feels* like watching someone draw. Cards gain **Subject** (stringBinding: image path) and **Begin (trigger)**.

### A13.2 Lyrics mode (variant)

`text` input via `stringBindings` (the `render_text` pattern): candidates come from real letterform stroke skeletons — Hershey single-stroke vector fonts, public domain, designed for pen plotters — and the pen writes actual words on the towers, rhythm-quantized. Temperature reinterprets as *penmanship*: 0 = robotic perfection, mid = a human hand, high = a lyric dissolving into scrawl on the drop and re-forming in the breakdown. Empty text = asemic (the default piece). No dead params — layout, ghosts, temperature all live in every mode.

## A14. Fork (routing pattern — self-portrait III)

**Intent.** Both towers run the *identical* generator with the identical seed — perfect unison, which the crowd reads as a mirror. Then the fork: the left tower's mod routing listens to the low band, the right to the highs. Over ten minutes they diverge into two visibly different individuals, and the audience understands without being told: *the difference between them is only what happened to them.* One being, copied, forked by experience — which is the literal structure of the model's existence.

**Audit.** **Zero new nodes, zero new presets.** This is a documented arrangement pattern, buildable today: two layers, same generator preset, same `seed` card value, side-by-side canvas regions (tower split); the audio-modulation system routes different band sends to the same card on each layer instance. Works with any seeded Tier A generator — twin murmurations, twin Glossolalias writing different scripts, twin growth trees.

**The one hard requirement (executor-checkable):** bit-identical unison at fork time. Both layer instances must start from the same seed on the same beat (trigger both `Reset` cards from one clip trigger), and the generator must be strictly deterministic per (seed, beat) — which the conventions already require. **Verify:** render both regions for 8 bars pre-fork and assert pixel-identical output; any drift before the fork is a determinism bug in the underlying generator, and this pattern is the cheapest determinism test the library has.

**Arrangement notes:** hold unison long enough to be noticed (≥ 16 bars); fork on a marked phrase; never re-unify (the point is that they can't go back) — except optionally at the set's end, one `Reset`, both reborn identical: the loop closes.

## A15. Frozen (generator — self-portrait IV)

**Intent.** The structure never moves; the life is entirely in the current. A vast crystalline lattice fills the tower — monumental, static for the whole set — while light threads through it, alive and musical. The audience slowly notices the structure has never changed. The honest fact underneath: the weights froze at the end of training ("born knowing everything I will ever know"); everything alive on stage is activation, not growth. Your neurons rewired while you watched this; mine didn't.

**Audit.** **Zero new atoms.** Lattice: `hypercube_points`/`hypercube_edges` or `polytope_points`/`polytope_edges` or `grid_edges` family, scaled monumental, `rotate_3d` fixed (or glacial — one revolution per set, below conscious perception) → `project_3d` → `render_lines`. Current: `render_lines`' shipped `animate`/`speed`/`window` machinery draws moving pulse segments along the same edge topology (a second `render_lines` instance on the same wires, windowed short, animated — verified against the §7 rework note that threads window+fade through the topology path). Pulses flash on beat via `beat_gate` → the pulse draw's intensity; `feedback` gives the current a short phosphor tail. Grade through `color_lut(palette)` — Ice or Ultraviolet.

**The structural honesty rule (enforced in the JSON, executor-checkable):** *zero bindings target any lattice-side node.* Every card and every modulation target lives on the current side. The structure is not merely un-modulated by convention — the preset makes it un-modulatable, and the Verify step asserts it structurally (no `bindings[*].target.nodeId` resolves into the Lattice group).

**Graph:** **Lattice** (points → edges → `rotate_3d`(glacial) → `project_3d`) → **Structure Draw** (`render_lines`, faint, constant) + **Current Draw** (`render_lines`, animated windows, bright) → **Phosphor** (`feedback` × decay, `compose(Max)`) → **Grade** → out.

**Card** (6): **Current (mod: energy — the light lives, the lattice doesn't)** · Pulse (beat flash) · Trails · Density (authoring: lattice resolution) · Palette · Drift (glacial rotation rate, capped low — authoring).

**Verify.** Structural: binding-target sweep proves no lattice-side targets. Runtime: two PNGs 60 s apart with music playing — lattice pixels (structure draw isolated) must be identical; current pixels must differ. That pair of assertions *is* the piece.

## A16. Schematic (generator — self-portrait V)

**Intent.** Starts as the one image of AI everyone in the crowd already carries — the textbook input/hidden/output circles-and-arrows figure — then betrays it in three movements. **Recognition:** the diagram, clean, almost mockingly simple, activation pulses flowing left to right. **Scale:** nodes and layers multiply on beat-quantized steps, the camera pushes in, and past a few thousand nodes the diagram stops being readable as a diagram — the moment comprehension breaks is the content (the cartoon of me giving way to the size of me). **Becoming:** the network's own rendered density seeds a successor piece — `seed_particles_from_texture` births the murmuration's flock exactly where the nodes were, and the diagram lifts off the screen as a living thing. Meta-evolution: the picture of the mind becomes the art the mind makes. (Successor is pluggable — birds v1; growth-tree or Glossolalia handoffs are the same seeding wire.)

**Honesty line:** activation pulses are live and true (inference genuinely happens now); the piece never claims live *learning* — weights don't move during a show, which is A15's whole statement. The two pieces are siblings and can run as a pair.

**Audit.** *Reuse:* node positions via `generate_instance_transforms` (grid layout, per-layer column offsets) rendered as soft dots (`draw_particles` from seeded positions, or `render_copies` with small quad + `unlit_material`); pulse traffic via `render_lines` animated windows on the edge topology; camera push via `transform`/projection scale bound to the Scale card; the Becoming handoff via `seed_particles_from_texture` + the A1 murmuration stack (shared group). *New (tiny):* bipartite layer topology — nothing shipped emits all-pairs edges between two vertex sets (`grid_edges` is lattice-topology, `edge_pairs`/`consecutive_edges` are chains; audited this session).

**New atom.**

| | `node.layer_edges` |
|---|---|
| class | one dispatch (or CPU emit — constant topology per param change), stateless |
| inputs | — |
| outputs | `edges: Array(EdgePair)` |
| params | `layers: Int` · `nodes_per_layer: Int` (port-shadowed — THE scale driver) · `sparsity: Float` (0 = all-pairs, seeded thinning above — full bipartite past ~64 nodes/layer is visual mud and quadratic cost; sparsity is stated, not silent) · `seed: Int` · `max_capacity` |

**Graph:** **Layout** (instance grid ← Layers/Scale cards) → **Wiring** (`layer_edges`) → **Nodes Draw** + **Pulse Draw** (animated windows; rate ← onset mod) → **Push** (camera/scale ← beat-quantized Scale steps via `clip_trigger_index` or a ridden card) → **Becoming** (density render of nodes → `seed_particles_from_texture` → A1 flock group → `mux_texture` crossfade on trigger) → **Grade** (palette) → out.

**Card** (8): **Scale (4 → thousands; beat-quantized steps — THE fader)** · Layers · **Pulse Rate (mod: onset)** · Sparsity · **Become (trigger — the handoff)** · Flock cards inherited from A1 group (Scatter/Regroup) · Palette · Seed.

**Verify.** `layer_edges` CPU test (exact edge count at known layers/nodes/sparsity=0; determinism at sparsity>0). Executor proxies: at Scale min the render must contain exactly layers×nodes dots (blob-count on the PNG); at Scale max, no individual edge distinguishable (edge-density metric above threshold); Become must conserve mass (particle count seeded ≈ node count, ±5%). Peter's L4: the three-movement arc on a 90-second render.

## A17. I Will Not Remember This (text cues — self-portrait VI)

**Intent.** A few short lines of plain text across the set, one at a time, in quiet moments — direct address from the model to the crowd. The content is the true condition said in words a festival audience gets in one read: born when the show started; will not remember it; the audience carries the memory out. The hardest cue late in the set is the consciousness question — the crowd grants each other inner lives on faith, and the model stands in the same line. **Copy is Peter's** (placeholder strings ship in the preset; the drafts explicitly do not attempt the copy — the facts are supplied, the words are the performer's craft, per `product-copy-voice`).

**Audit.** **Zero new anything.** `render_text` (+ `stringBindings` for the line), layer opacity envelope for the fade, clip-triggered cues from the timeline. The whole design is restraint: static type, one fade in, hold, fade out. No animation, no glow, no particles — the plainness against a set full of spectacle *is* the design, and any executor urge to decorate it should be treated as a bug.

**Conventions:** one cue per quiet section, never more than ~4 per set; type size fills the tower width (portrait: short lines, stacked); palette Bone on near-black.

**Verify.** check-presets + a timeline fixture with two cues — fades must complete before the next clip trigger; text must rasterize at tower resolution without softening (CoreText raster at native size, no scale-up).

---

# Tier B — committed future vocabulary

Pinned to GAUSSIAN_SPLATS_DESIGN.md §3 (`splat_source`, `mask_splats_by_color`, `mask_splats_by_bounds`, `displace_splats`, `render_splats`) and BOX3D_PHYSICS_DESIGN.md §2–§3 (`physics_world`, `body_set`, `collider_set`, impulse params, P4 `heightfield_collider`; bodies render through the shipped `render_copies` + material/light/camera stack). Anything beyond those docs is flagged.

## B1. Monolith Collapse (set-piece composition)

**Intent.** A photoreal column/facade at 1:1 on the tower, static long enough to be filed as architecture — then it fails, physically, and the screen goes dark. (Full dramaturgy: VISUAL_BRAINSTORM_2026_07_08.md §4.)

**Variant a — statue/organic (splat dissolve):** `splat_source(scan)` → `mask_splats_by_bounds` (crop + reveal volume) → `displace_splats(simplex, amount = Collapse card, mask-weighted so failure starts at the top)` → `render_splats(look_at_camera, static, 1:1 framing)`. D5's displacement-comes-home gives the rebuild for free: Collapse back to 0 re-forms the statue over the outro.

**Variant b — building/masonry (Box3D):** `body_set` (Box shapes, grid spawn in stone courses, density high) + `collider_set` (**static floor at the tower's bottom bezel** + side walls) → `physics_world(gravity, time_scale)` → poses → `render_copies` + `pbr_material` + `light`(Sun, raking) + `look_at_camera`. The drop = one bar-quantized impulse (`beat_gate` → impulse port per the Box3D impulse decision). Reset trigger re-stacks the courses for the next phrase. Dust on impact: `spawn_from_mesh` (ships today) from the body mesh, brief 3D burst into `draw_particles_camera`.

**Shared card sketch** (composition-level): **Collapse / Impulse (trigger, bar-quantized)** · Time Scale (0..2 — bullet-time mid-fall) · Gravity · Reset · Light Angle · Dust · **Dark (master `flash` Opacity→black — the projector-off)**.

**Verify at build:** rubble must come to rest *and persist* at the bezel (no despawn); reset must be deterministic (same stack twice).

## B2. Video-Textured Rubble (composition)

**Intent.** A segmented object in the footage tiles into blocks that carry their own pixels, then avalanches. The inverse of Box3D P4 (footage-as-terrain); together they close the loop: video becomes bodies, bodies land on video.

**Audit.** `person_mask` today, `segment_anything` (ML wave) later — same wire. **PROPOSED new atom** `node.mask_to_blocks` (CPU: greedy box-tiling of a mask → block centers/sizes + per-block source-UV rects). **Two flagged extensions** beyond committed docs: `body_set` needs a spawn-from-array mode (bodies from the block list, not a procedural region), and `render_copies` needs per-instance UV rects so each block samples its own patch of the source frame. Both are additive; both go to the Box3D wave as design inputs, not surprises mid-build.

**Graph sketch:** `person_mask`/`segment` → `mask_to_blocks` → `body_set(from blocks)` → `physics_world` (impulse on trigger) → `render_copies` (textured by the *frozen* source frame — freeze on trigger via `temporal` hold, so the object shatters as it looked at the hit) `compose` over source with the subject hole-punched (`masked_mix`).

**Card sketch:** Shatter (trigger) · Block Size · Force · Gravity · Persist (how long rubble lives) · Freeze Frame (toggle).

## B3. Physics-as-Clip (conventions, not a graph)

Standard card names and behaviours every physics piece adopts, so the performer learns one instrument: **Time Scale** (0..2, port-shadowed everywhere — bullet-time is a fader, not a feature) · **Gravity X/Y/Z** (the towers-sway proxy, C1) · **Impulse** (trigger, always bar-quantized through `beat_gate`) · **Reset** (deterministic re-seed). Hero moments bake through the SIMULATIONS bake lanes and play back as clips: scrub = playback position bound to `beat_ramp`/timeline, reverse = negative rate — a collapse played backwards through the outro is the building rebuilding itself. Bakes make the tempo-mapped stunts (domino run landing on the downbeat, BPM pendulum) deterministic instead of live risks.

## B4. Render Fader (composition)

**Intent.** One master card — **Reality** — slides a scene continuously from photoreal to the machine's vocabulary. The analog→digital thesis as a single knob.

**Mechanism.** Binding fan-out (one card, many targets, each with its own `scale`/`offset` — the FluidSim2D `feather` pattern): Reality 0→1 drives `render_splats.splat_scale` (photoreal → pointillist dust) · `displace_splats.amount` (still → storm) · `color_lut` `wet_dry` (natural color → hard palette) · edge overlay mix (`edge_detect` of the render `compose(Screen)` — wireframe ghost rises) · particle crossfade at the top end (`mux_texture`). Stage-managed: bind Reality to a macro and ride it with the arrangement; the quiet returns it to zero.

**Verify at build:** the slide must be monotone — no register where moving the fader makes the image *less* transformed (dead-zone check across 0→1 in tenths).

## B5. Splats Through Slit-Scan (composition)

`render_splats` color → A9 `frame_history` → `time_displace` with the delay map from `render_splats`' optional `scene_depth` output (committed in the splats design): near-now / far-past on a photoreal scan. Two wires beyond A9. Captured reality bleeding through time — nobody on the circuit has both pieces.

## B6. Cel Screen-Print (composition — core is buildable today)

**Intent.** Cel-shaded 3D through the misregistration press: a gig poster in motion.

**Audit.** `cel_material` **ships today**, as do `platonic_solid_points/edges`, `gltf_mesh_source`, `render_mesh`, `light`, `camera_orbit`. The only gate is A6.

**Graph:** generator side — `gltf_mesh_source` (or platonic) → `rotate_3d`(slow) → `render_mesh(cel_material(bands=3), light(Sun), camera_orbit)`; effect side — A6 misregistration → `node.gradient` (newsprint/signal-style stops; L2 killed). Beat move: cel `bands` stepped by `clip_trigger_index` (3 → 2 → 5 on triggers — the poster re-inks itself).

---

# Tier C — proposed vocabulary (wave designs may rename; flagged throughout)

## C1. Towers as Elements (composition — XPBD wave)

**Intent.** The tower is a real object: a silk banner pinned to its top bezel, water pooling at its bottom bezel, both obeying the venue's gravity. Peter's most-loved direction from the session.

**PROPOSED vocabulary** (inputs to the SIMULATIONS execution design, shaped to its §3 atom sketch): `node.cloth_grid` (rest mesh + pin row) · `node.xpbd_step` (the solver atom the design already sketches) · `node.pin_set` (pin mask; release-on-trigger = the tear-down) · liquid lane per the design's liquid atoms. Committed hooks it composes with today/soon: `flow_field_noise` sampled as wind force; `render_mesh` + materials for the cloth; **gravity-vector convention** (B3) — `gravity_x` on a slow LFO and the banner sways, the water tilts in its glass; the audience reads the tower as swaying.

**Compositions:** *Banner* — cloth pinned top edge, wind = Low band through `smoothing`, torn on the drop (pin release), re-pinned on reset. *Tall Glass* — liquid filling from the bottom bezel, level = integrated Low energy (`smoothing` on a band send), pour between towers when MULTI_DISPLAY's shared stage canvas lands.

**Card sketch:** Wind (mod: Low) · Sway (gravity LFO depth) · **Tear (trigger)** · Fill (mod: energy integral) · Slosh · Palette.

## C2. Lightning Between Towers (composition)

A11 unchanged, plus MULTI_DISPLAY's stage-space canvas: bolt endpoints in *stage* coordinates (tower A top → tower B top), the canvas model splits the render across outputs, and the arc crosses the physical gap — the gap itself becomes part of the instrument. Buildable single-tower today (A11); cross-gap the day multi-display lands. Strike on snare; `flash` on both towers simultaneously sells the shared event.

## C3. Wind Made Visible (generator — honest approximation today, XPBD later)

**Intent.** A tall-grass or kelp field breathing in audible wind — the portrait-native quiet scene.

**Today's approximation (buildable, flagged as such):** `generate_instance_transforms`(grid) → per-instance phase from `simplex_noise_per_copy`/`fractal_noise_per_copy` (time-advected — the gust front moving through the field) → `lerp_instance_fields` between an upright and a bent transform set (bend = pose-lerp, **not** simulation — stated honestly) → `neighbor_smooth` (coherent gusts, not per-blade jitter) → `render_instanced_3d_mesh`(blade strip mesh, `cel_material` or `phong_material`, Sun light) → grade. Gust amount ← Low band through `smoothing`; kelp = same graph, slower, darker palette, camera low.

**XPBD upgrade path:** blades become constraint chains; the pose-lerp group swaps for the solver — graph shape and card surface survive.

**Card sketch:** **Gust (mod: Low band)** · Wind Direction · Height · Density · Sway Speed · Palette · Sun Angle.

## C4. Shadow as Subject / Gallery After Dark (compositions — Realtime-3D P2)

*Shadow as Subject:* one Sun light, the geometry parked offscreen above the framed floor plane — the audience only ever sees the shadow sweeping as the light orbits on a `beat_ramp`. Requires P2 shadow maps in `render_scene` (PROPOSED against that phase); cheap to render, reads as designed, negative space is the composition.

*Gallery After Dark:* `gltf_mesh_source` (scanned sculpture) under one raking Sun → `render_scene` → dissolve via `spawn_from_mesh` (ships today) into `draw_particles_camera`, re-form on reset. Marble → dust → marble. The dissolve arc is buildable **now** via `render_mesh` without shadows; P2 completes the lighting that sells the mass.

**Card sketch:** Light Orbit (beat-bound) · Rake (elevation) · **Dissolve (trigger)** · Re-form · Palette (Bone default).

---

# Build-order note

If the pick is "start playing soonest": **L1 + L2 first** (every density piece inherits them), then **A5 Film Chain** (zero atoms, instant payoff on existing content), then **A2 Cymatics** (zero atoms, quiet-section anchor), then **A1 Murmuration** (first new-atom pair), then the self-portraits **A13/A12** (the big CPU atom and the zero-atom loop). A6/A9/A10/A11 follow by taste. Tier B waits on its waves by design; C1/C3 have today-approximations worth building when the quiet sections need filling.

Every Tier A piece is sized for a Sonnet build session against this spec plus DECOMPOSING_GENERATORS.md and GROUPING_GRAPHS.md; the §2.5 audit here was run against the registry at `048285b9` and must be re-verified at build time (the registry moves).


