# Competitive Steal Pass

**Status: CLOSED (decisions pinned) · 2026-07-02 · Fable queue #10**

A walkthrough of the live-visuals field — Resolume Arena/Avenue, TouchDesigner, Notch, VDMX, Smode, MadMapper, Millumin, Modul8, EboSuite, grandVJ, Synesthesia — asking one question per feature: *is this worth stealing for MANIFOLD?* Verdicts are **steal** (S1–S8), **reject** (R1–R4, decision-log material — don't re-propose), or **already covered** (§1).

This is a survey-and-verdict doc, not a design doc. Each steal gets a rough shape and a home; the two big ones (S1, S2) get their own design sessions.

---

## 1. Already covered — don't envy what's already designed

| Their feature | Where MANIFOLD has it |
|---|---|
| Clip grid / media bins (Resolume decks, VDMX pages, Modul8) | `docs/SESSION_MODE_DESIGN.md` — layer×scene grid |
| Multi-output / advanced output routing | `docs/MULTI_DISPLAY_DESIGN.md` — island atlas, Stage domain |
| Parameter automation / envelopes | `docs/AUTOMATION_LANES_DESIGN.md` |
| Body/pose/face input (TD MediaPipe, Notch camera FX) | `docs/ML_NODES_DESIGN.md` |
| Exposed macros / dashboards (Resolume dashboard, Notch exposed properties) | Component macros → card bindings, `docs/COMPONENT_LIBRARY_DESIGN.md` |
| Node graph authoring (TD, Notch, Smode) | The graph runtime + ~188 primitives |
| Audio-reactive everything (VDMX data sources, Synesthesia audio uniforms) | manifold-audio features + binding unification + audio triggers |
| BPM sync, beat quantize (Resolume) | Beats-primary time model — deeper than any of them |
| DMX/Art-Net out, DMX-in triggers | manifold-led + multi-display tech rider decisions |
| User shader surface (Resolume Wire-ish, TD GLSL TOP) | `wgsl_compute` — a real live-show surface |

Where MANIFOLD is already ahead of the field (positioning, not work): a real beats/bars **arrangement timeline** (no VJ tool has one), **.als show import** (`docs/ABLETON_SHOW_SYNC_DESIGN.md`), the **understudy crash watchdog** (`docs/GIG_RESILIENCE_DESIGN.md` — nobody ships this), and **AI-native authoring** (`docs/MCP_INTERFACE_DESIGN.md`). Notch's remaining moat is simulation content quality (fluids, high-end particles) — a content-roadmap item, not a UX steal.

---

## 2. Steals

### S1 · Projection mapping — warp, edge blend, masks, slices
**From:** Resolume Arena (Advanced Output), MadMapper.
**Verdict: steal — the biggest item in this pass. Own design doc next (queue #11).**

> Peter: "projector warp and blend is critical I think. Projection mapping these visuals will look incredible."

What it is: per-projector output transforms — corner-pin homography, bezier/mesh warp for curved surfaces, feathered gamma-correct edge blend between overlapping projectors, polygon masks, and slices (stage region → warped output region, many-to-many). With this, the instrument plays *the room* — booth fronts, ceilings, set pieces — not just rectangles.

What exists: `docs/MULTI_DISPLAY_DESIGN.md` gives displays-as-islands with zero gap pixels and venue profiles keyed by display UUID. Mapping extends both: warp meshes / blend curves / masks are **venue-profile data** (same show, different room), and GPU-wise the warp pass is one textured-mesh draw per output — cheap.

The design question for #11 (not answered here): projector islands need **overlapping** stage samples for blend zones, so a projector's island is a stage *region* (overlap allowed, atlas grows), followed by an island→framebuffer warp pass. That must be reconciled with the current zero-gap island model, plus the calibration UX (on-output test grids, live control-point nudge, per-venue save).

### S2 · Perform surface builder
**From:** TouchDesigner perform mode, VDMX modular workspace.
**Verdict: steal — as a layout layer over cards, not a DIY UI toolkit. Own light design session.**

> Peter: "build your own perform surface is a cool feature!"

Pick which widgets appear on the perform view — macro knobs, XY pads, clip pads, next-clip preview (S3), tap tempo, layer opacities, audio meters — arrange them on a grid, saved per project. Everything underneath already exists (cards, binding unification, session pads); the steal is only the arrangeable layout + widget registry. TD makes you build panels from raw components; MANIFOLD snaps existing cards onto a grid. Push-style, not toolkit-style.

### S3 · Cue / preview bus
**From:** Resolume preview monitor.
**Verdict: steal — small, pure stage value.**

See the armed clip playing on the control display *before* firing it. Shape: a preview context renders the armed slot's content at low res (the node-preview infrastructure is the precedent), shown as a perform widget (S2) or dock panel. No output-side plumbing — control display only.

### S4 · Clip transitions on launch
**From:** Resolume per-layer transition time.
**Verdict: steal — fold into session mode backlog.**

Launching a slot crossfades from the currently playing clip over N beats (or snaps at 0). Session mode's `PendingSlotLaunch` already stages the switch at the quantize boundary; the addition is a per-layer `transition_beats` and a fade window where both chains render (bounded 2× cost during the fade only). Beats, not seconds — this is MANIFOLD.

### S5 · Autopilot / follow actions
**From:** Resolume autopilot, Ableton follow actions.
**Verdict: steal — pull forward from session mode's deferred list.**

Slot finishes → auto-fire next / random / back to arrangement. Explicitly deferred in `docs/SESSION_MODE_DESIGN.md` non-goals; this pass promotes it to the session-mode v2 backlog. Gives unattended sections (doors music, breaks) and generative set flow.

### S6 · Ableton Link + Link Audio
**From:** the entire Link ecosystem; Link Audio shipped in Live 12.4 (May 2026).
**Verdict: steal — both halves.**

- **Link (tempo/beat/phase/transport):** MANIFOLD's beat clock can join any Link session — DJ gear, other laptops, zero setup. Complements the AbletonOSC bridge (which stays the deep perform-side integration).
- **Link Audio (named audio channels over LAN):** a third source family in `manifold-audio` next to cpal inputs and CoreAudio taps — Ableton master/stems arrive over the network **aligned to the Link beat timeline**. No cables, no output-tap hacks, works from a second machine (the two-laptop rig). Beat-aligned audio means analysis features line up with the timeline for free.
- **Licensing (pinned):** Link SDK is GPLv2 or a free proprietary licence from Ableton (link-devs@). MANIFOLD ships closed-source → Peter requests the proprietary licence; one request covers Link + Link Audio. Implementation is Sonnet work, gated on the licence.

Related, already mapped (2026-06-03 research): the **Extensions SDK** (Live 12.4, Node.js inside Live) is the compose-side door — full Set data model, `MidiClip.notes`, named drum pads, `renderPreFxAudio` offline stems. No realtime, no observers; complements OSC, and offers show-sync an alternative import path plus baked stems that .als parsing can't produce.

### S7 · ISF shader import
**From:** VDMX (VIDVOX's Interactive Shader Format), also loaded by Resolume and Millumin.
**Verdict: steal — the library, via naga, with an MCP fallback.**

ISF = one `.fs` file: a JSON header declaring params + a GLSL fragment shader. Thousands of free community effects (isf.video). The importer:

1. Parse the JSON header → spec sheet params (same surface as `wgsl_compute`).
2. Expand ISF macros (`IMG_NORM_PIXEL`, `RENDERSIZE`, `TIME`, …) into plain GLSL.
3. naga GLSL frontend → MSL (naga already ships in the build for WGSL→MSL). Multi-pass / `PERSISTENT` buffers map onto existing per-port state.

Honest caveat: naga's GLSL frontend is its weakest, and community ISF is sloppy GLSL 1.2 — a fraction won't compile. First implementation step is a **corpus experiment**: run the isf.video library through the pipeline, measure the pass rate. Shaders that fail get ported by the MCP agent on demand ("here's an ISF file, make it a MANIFOLD effect") — the agent path works even if the importer never ships.

### S8 · Per-node example graphs
**From:** TouchDesigner OP Snippets.
**Verdict: steal — content work, compounds with AI authoring.**

Every primitive ships a tiny working example graph, openable from the node browser/descriptor UI. Slots into the node-descriptor UX work (friendly names, taxonomy, tooltips) and doubles as few-shot corpus for the MCP agent — one artifact, two consumers.

---

## 3. Rejects — logged so they stay rejected

- **R1 · Build-your-own modular UI everything (VDMX).** The whole app as movable panels. Take the layout layer only (S2); reject the toolkit. MANIFOLD's fixed editor + arrangeable perform surface is the right split — DIY workspaces tax every user to serve power users.
- **R2 · Theater cue stacks with GO button (Millumin).** MANIFOLD shows are Ableton-driven; cues + show-sync + trigger clips cover it. A parallel cue-stack entity re-introduces the trigger-lane concept already rejected in show-sync D1.
- **R3 · Media-server block export (Notch → disguise/d3).** Exporting MANIFOLD content to run inside other servers is a different product. Components + macros give the same authoring concept in-app.
- **R4 · SMPTE timecode chase.** Already closed — timecode locks the score, never scrubs the render; Art-Net in for lighting-desk integration. See decision log / multi-display tech rider.

---

## 4. Dispositions

| Item | Home | Effort tier |
|---|---|---|
| S1 mapping | **New design doc — queue #11, next Fable session** | Fable design → Sonnet phases |
| S2 perform surface builder | Own light design (Fable-quick or strong Sonnet brief) | Small-medium |
| S3 cue/preview bus | Spec'd enough here; perform-surface widget | Small (Sonnet) |
| S4 clip transitions | Session mode backlog, spec'd enough here | Small (Sonnet) |
| S5 follow actions | Session mode v2 backlog | Small (Sonnet) |
| S6 Link + Link Audio | manifold-audio + clock; **gated on licence (Peter requesting)** | Medium (Sonnet) |
| S7 ISF import | Corpus experiment first, then importer; MCP fallback regardless | Medium (Sonnet) |
| S8 node examples | Node-descriptor UX + MCP few-shots | Content (any model) |
