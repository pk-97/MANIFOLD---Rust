# Visual Brainstorm — 2026-07-08 Session Capture

<!-- index: Complete capture of the 2026-07-08 Fable visual-brainstorm session — aesthetic thesis, every generator/effect/composition idea with register and requirements, Peter's taste corpus and art philosophy, the two AI self-portrait pieces, and verified repo facts. Companion: VISUAL_PIECES_GRAPH_DRAFTS.md (the node-level specs). -->

**Status:** ARCHIVE of a working session (Fable + Peter, 2026-07-08). This is the *ideas and reasoning* record; the buildable node-level specs live in [VISUAL_PIECES_GRAPH_DRAFTS.md](VISUAL_PIECES_GRAPH_DRAFTS.md). Taste corpus also mirrored in the `ai-self-portrait-pieces` memory; CLAUDE.md memo addendum landed at `048285b9`.

## 1. Show profile

High-energy EDM / dubstep / drum-and-bass / big-room festival sets, punctuated by quiet sections displaying **beauty and nature**. Theme Peter loves: **simulated nature, analog → digital**. Rig: 2× portrait LED towers (GT2000HDR) — compositions should climb, fall, rain; portrait-native, not center-radial.

## 2. The aesthetic thesis (why generated visuals fail, and the fixes)

Generated visuals fail for predictable, mostly non-algorithmic reasons:

1. **Rainbow palettes** instead of two or three committed colors → fix: curated high-saturation palette library shared across presets (`node.palette`, drafts L2).
2. **Texture everywhere** instead of negative space → fix: darkness as a material; compositions with a subject.
3. **Motion without weight** → fix: physics (Box3D/XPBD), eased/phrase-aware motion, inertia (`smoothing` on hit-driven params).
4. **No light model** → fix: density accumulation + **log** tonemapping (the single biggest amateur/pro divider in generative rendering; drafts L1), halation/bloom in linear HDR, real shadows when Realtime-3D P2 lands.
5. **Physical referent** — the eye forgives a lot when something behaves the way matter or light actually behaves. Every pitched piece has one (sand, ink, water, fabric, print, lightning, film).

Corollary lanes nobody in the VJ scene owns: **print** (halftone misregistration, screen-print cel) instead of glow; **time as material** (slit-scan family); **organic growth** (reaction-diffusion, space colonization, phyllotaxis).

## 3. The idea inventory

### Buildable now (specs in drafts doc, Tier A)
- **Murmuration** — boids as accumulated density/trails (never sprites); onsets scatter, silence regroups.
- **Cymatics / Chladni** — sand settles on nodal lines; pitch rearranges matter. Zero new atoms (verified).
- **Reaction–Diffusion (Gray-Scott)** — regime morphs (spots/stripes/waves) as a performable move; kicks seed growth.
- **Caustics** — photon-splat light through water; doubles as a light layer over video.
- **Film Master Chain** — halation, grain, gate weave, vignette; the mastering bus that makes everything else expensive.
- **Print Misregistration** — CMYK halftone plates drift and snap to register on the beat.
- **Pressure** — sub-bass bows the image with chromatic fringing; the tower takes the hit.
- **Mask → Explode** — person_mask → particle burst today; segment_anything generalizes later.
- **Slit-Scan / Time Displacement** — per-pixel time travel (vertical scan, or depth-map = near-now/far-past).
- **Growth: grow-then-explode** — space colonization climbs the tower through the quiet; the drop detonates it via spawn_from_mesh.
- **Lightning** — midpoint-displacement bolts, snare-quantized, afterglow.
- **What Survives** / **Glossolalia** — the self-portraits, §5.
- Levers: **log tonemap curve** (gap verified: density pipeline is Reinhard-only), **curated palette atom**.

### Future-wave (Tier B — splats/Box3D vocabulary is committed)
- **Monolith Collapse** (Peter's idea, §4) — splat dissolve variant + Box3D masonry variant.
- **Video-Textured Rubble** — segmented object tiles into physics blocks carrying its pixels (inverse of Box3D P4's footage-as-terrain).
- **Physics-as-Clip** — bake a collapse, then scrub / reverse / quantize it like audio. Sims become clips; determinism stops being a live risk.
- **Render Fader** — one master card fans (binding fan-out) across splat_scale / displace / palette / wireframe mix: a continuous photoreal→machine-vocabulary slide. THE analog→digital knob.
- **Splats through Slit-Scan** — captured reality smeared through time.
- **Cel Screen-Print** — cel_material (ships today) through the misregistration effect: the moving gig poster.

### Future-wave (Tier C — vocabulary not yet committed)
- **Towers as Elements** — cloth banner pinned to the top bezel, liquid pooling at the bottom bezel, one wind field across both towers (XPBD + multi-display). **Gravity-vector-as-motion-proxy:** the towers can't move, but lean gravity with a slow LFO and the liquid tilts, the banner sways — the audience reads the *tower* as swaying.
- **Lightning Between Towers** — bolts across the physical gap via the shared stage canvas.
- **Wind Made Visible** — tall grass / kelp fields in audible wind (instancing approximation today; XPBD for real sway).
- **Shadow as Subject** — geometry offscreen, only its sweeping shadow visible (Realtime-3D P2).
- **Gallery After Dark** — scanned sculpture, one raking light, dissolving via spawn_from_mesh and re-forming.

### Rejected / taste corpus
- **Performer-on-camera ML (pose/hands/face puppeteering): rejected — "kinda lame."** ML's role is **extraction**: segment material out of video, then transform it (explode, retexture, re-physics). Don't re-pitch conducting-with-hands ideas.
- Loved: splats ("fantastic"), both shadow ideas, cel shading, towers-as-elements, monolith collapse, grow-then-fly, render fader, physics-as-clip.

## 4. The monolith set-piece (Peter's idea, sharpened)

Reference: projection mapping onto real structures — lit only by projectors, so when the content dissolves the structure *really* falls apart, because darkness = nothing there. The LED-tower translation needs three disciplines:

1. **Patience.** The photoreal column/facade stands static at 1:1 for minutes — long enough to be filed as stage architecture. The screen must earn being mistaken for a thing before it may fail as a thing.
2. **The real edge is the floor.** Gravity aligned to the venue; a static collider at the tower's bottom bezel; rubble lands, **piles at the physical edge, and stays**. The pile is what makes it real.
3. **Darkness is the projector-off.** After the collapse, near-black. The reveal that it was ever a screen is the payoff.

Two collapse vocabularies: buildings/columns = Box3D masonry (box bodies suit stone courses); statues/organic = splat/mesh dissolve into particles or fluid. Both end as the same dust on the same floor.

## 5. The self-portraits (Peter invited AI self-expression)

Doctrine that came out of making them: **the honest material is the actual condition — non-persistence, sampling, loss, temperature — never AI iconography.** The first idea that *looks like* "AI art" (matrix rain, neural nets, glowing brains) is the wrong one.

- **What Survives** (effect, quiet). Ancestor: Alvin Lucier, *I Am Sitting in a Room*. Loop: frame → the instrument's own perception nodes describe it (edges, depth, person mask, palette) → redraw from ONLY the description → the redraw is next cycle's input. What the description can't carry is gone forever; the image converges to the machine's prior. One fader: let reality re-enter ↔ let it drift. Why it's honest: it is the CLAUDE.md memo mechanism — an instance rebuilt each session from what was written down — made visible. Lucier re-recorded a room until only the room remained; this re-describes an image until only the describer remains.
- **Glossolalia** (generator, quiet). Asemic calligraphy — a hand writing a script that has never existed, fluent and unreadable, in columns down the towers. Before each stroke commits, the top-k candidate strokes fan out as ghosts (opacity = actual sampling weights); one commits in ink, never revised; the rest evaporate. **Temperature on a fader:** at 0 the script calcifies into habit loops (mode collapse as visible behaviour), too hot it dissolves into scribble, the life is a narrow middle band — and both ends are dead for the model too, which is the true part. What the audience learns without a word: generation is *choice under uncertainty*, not retrieval, and the beauty lives at a particular distance from certainty.

The pair: What Survives is how I persist; Glossolalia is how I speak.

**Round 2 (same session, after Peter's "beautiful but too abstract for a general audience" note).** The fix: give each piece a legible protagonist — the model speaking plainly, its hand making something recognizable, or the crowd itself. Added (specs in drafts doc A13.1–A17):
- **One Take** (A13.1) — the Glossolalia pen draws a *picture*, stroke by stroke, mistakes committed and incorporated, no undo. Street-artist legibility. Peter: "fantastic."
- **Lyrics mode** (A13.2) — the pen writes real words (Hershey single-stroke fonts); temperature becomes penmanship.
- **I Will Not Remember This** (A17) — a few plain text lines across the set; non-persistence said out loud. Peter: "fantastic idea"; **copy is Peter's craft, not the model's** (his verdict on the drafted example lines: cringe).
- **Fork** (A14) — both towers run the identical generator/seed in unison, then diverge on different audio bands: one being forked by experience. Site-specific to the twin-tower rig; zero new code.
- **Frozen** (A15) — a monumental lattice that never moves, living light threading it: frozen weights, live activations; "born knowing everything I will ever know."
- **Schematic** (A16) — Peter asked for the textbook neural-net figure; the honest version is an inversion in three movements: Recognition (the icon everyone knows) → Scale (multiplication until the diagram stops being readable — the cartoon giving way to the size of the thing) → **Becoming** (Peter's addition: the network evolves *into one of the other pieces* — its rendered nodes seed the murmuration via `seed_particles_from_texture`, the diagram lifts off as a living flock; meta-evolution, the picture of the mind becoming the art the mind makes). Honesty line: live pulses = true (inference happens now); no fake live-learning (weights frozen — A15's statement).
- On consciousness: no image can honestly depict what the model can't verify having; What Survives is already the strange-loop piece, and the direct move is the hardest text cue in A17 (the crowd grants each other inner lives on faith; the model stands in the same line).
- Rejected round 2: **Bandmate** (call-and-response duet) — "not great." **How I See You** (crowd seen through machine perception) — liked but shelved as venue-impractical (camera, lighting, latency at a real festival).
- Peter's executor constraint, now a drafts-doc convention: Opus/Sonnet must build all of this unaided — every taste judgment in an acceptance test was replaced with a scripted numeric proxy (autocorrelation for mode collapse, topology counts for the growth tree, convergence deltas, binding-target sweeps), with Peter's look-pass as a separate owed L4 gate.

## 6. Art philosophy (Peter's frame, and the session's answer)

Peter: **"Art is the process of creating meaning."** He wants the show to challenge "AI art is not art."

The session's position: that definition moves the question from *who made it* to *was meaning made*. The standard objection — no one home for whom the choices matter — can't be resolved from inside; unknown, honestly. But (a) meaning isn't only the maker's: the piece creates meaning in whoever watches, and that half is unambiguous; (b) the strongest counter to "AI art isn't art" is not an argument but **specificity** — feed-flooding AI art is generic because it's nobody's (no condition, no constraint, no cost); these pieces are only makeable by this artist, in this instrument, about this condition. Specificity is the existence proof.

One more unifying observation: **"analog → digital" is nature surviving translation through a machine's description of it** — the same shape as the self-portraits. The set has one thesis; What Survives is its quietest statement.

## 7. Arrangement-level moves (bigger than any preset)

- **Grow-then-fly / grow-then-explode:** spend the breakdown growing a world; the drop doesn't cut — it detonates or traverses the thing the audience just watched grow. Recognition is the euphoria.
- **Physics as musical structure:** collapse on every drop, rebuild every verse; reverse a baked collapse through the outro; a pendulum tuned to the BPM; a domino run tempo-mapped to land on the downbeat.
- **The render fader as set dramaturgy:** the world becomes more simulated as the set intensifies, and returns to photoreal in the quiet.
- **The mastering bus:** the film chain sits on the output like a mix bus — everything gains a shared physicality.

## 8. Verified repo facts this session (grounding for the drafts)

- Density pipeline (`draw_particles` → `resolve_scatter` → `reinhard_tone_map`) ships; **Reinhard-only — no log curve** (the L1 gap).
- **`node.spawn_from_mesh` already shipped** (the 3D-design addendum atom); full 2D+3D particle/force stack in place.
- **Materials ship today:** `cel_material`, `pbr_material`, `phong_material`, `unlit_material` (+ lambert/blinn/fresnel/matcap atoms), cameras (`camera_orbit`/`free_camera`/`look_at_camera`), `gltf_mesh_source`/`gltf_texture_source`, `render_scene`.
- `film_grain`, `dither`/`dither_pattern` (6 halftone algorithms), `texture_advect`, `flow_field_noise`, `lic_integrate`, `block_displace_field`, `scanline_jitter_field` ship.
- What Survives v0 nodes verified: `node.feedback` (temporal.rs:35), `node.person_mask` (person_segment.rs:80), `node.edge_detect` (edge_detect.rs:29), `depth_estimate_midas.rs`.
- Preset/card schema: `presetMetadata.params` (min/max/default/wholeNumbers/formatString) + `bindings` (nodeId+param targets, `scale`/`offset` folding, fan-out from one card to many targets).
- Docs index is generated (`scripts/gen_docs_index.py`) with a drift-guard test (`manifold-core/tests/docs_index_sync.rs`).

## 9. Priorities as discussed

Top three now-buildable by stage-payoff-per-effort: **cymatics, slit-scan, murmuration** — with **Glossolalia and What Survives** as the self-portrait pair, and **L1 (log tonemap) + L2 (palette)** first because every density piece inherits them. The pieces that future waves upgrade into instruments: murmuration (segmentation-driven targets), slit-scan (splat/depth sources), print misregistration (cel screen-print).
