# LED Strips — Play the Strips + Generalized Patch

**Status: IN PROGRESS — MVP (LED layer type + direct-drive patterns) briefed 2026-09-03, not built. Original
patch/sACN/island phases P1–P3 remain approved and unbuilt; the MVP defers them (see section 5b Deferred).**
**Prerequisites: none for the MVP (LED-resolution compositing machinery already exists — see MVP audit).
Original P1 (patch generalization) needs none; original P2 (strip island) rides the island model
from `docs/MULTI_DISPLAY_DESIGN.md` P1–P3 (P2 re-issuable, P3–P5 unbuilt as of 2026-09-03).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) and section 8 (Execution protocol (how a phase is run)) before starting any
phase. Conformance-hardened: section 1 is a 2026-07-03 snapshot — run the section 8.3 pre-flight
(re-verify `manifold-led` anchors, e.g. `rg -n 'LedSettings' crates/manifold-led/`)
before each phase; P2 runs after multi-display lands, so expect drift.**

Peter's directives (2026-07-02/03): strips get **"full strip control and going full blast …
they should accent the visuals"** (stage-content pass-through REJECTED: "not dense enough and
will look like a gimmick"); **"figure out how to 'play' the strips … chases, strobes,
patterns"**; **"generalise the LED art net stuff along side the SACN stuff too — at the moment
it's hard coded for me but should be general for all users"**; patterns are **2D** across the
strip array, not per-strip 1D.

---

## 1. What exists (audited 2026-07-03)

`crates/manifold-led/` is a working Art-Net pipeline shaped exactly like Peter's old rig
(a Unity `LedSettings.cs` port):

- **types.rs** — `LedSettings`: hardcoded rig constants (`DEFAULT_ARTNET_IP =
  "192.168.2.18"`, `DEFAULT_STRIP_COUNT = 8`, `DEFAULT_LEDS_PER_STRIP = 120`,
  `STRIPS_PER_SIDE = 4`), one `is_bgr` bool for color order, `StripAddressing::
  {PerUniverse, Packed}`, `blur_radius` (flicker smoothing), single-variant
  `ExternalOutputType::ArtNet`.
- **blit.rs** — edge-extend compute (WGSL) samples the stage texture's left/right bands into
  a tiny `Rgba8Unorm` texture (`strip_count × leds_per_strip`). This *sampling* is the part
  D1 demotes; the tiny-texture plumbing survives.
- **readback.rs** — async GPU readback of that texture (submit / try_read, non-blocking).
- **artnet.rs / controller.rs** — packet building, per-universe send, `blackout()`,
  lifecycle. Pre-allocated buffers, no per-frame allocation.
- **Trigger infrastructure** — trigger clips + named cues already exist (show-sync /
  session designs); patterns need zero new trigger machinery.

## 2. The reference rig

- **8× 2 m strips, 120× SK9822 each** (columns; count/length are patch config, not
  constants). SK9822 = APA102-class SPI pixels, high PWM rate — strobes and fast chases
  read cleanly ("very bright and responsive").
- **Controller: Suntech H807SA** (manual on file) — Ethernet Art-Net in, SPI pixel out;
  8 ports × 1024 px (960 used ≈ 12%); per-port IC type / pixel count / universe mapping
  configured **on the unit**. MANIFOLD's only job is to emit the universes the patch
  declares; controller internals never leak into the data model. **Art-Net only — no sACN**
  (manual mentions no E1.31); the design's sACN output exists for other rigs. SK9822 is
  not in the chip list — run it as **APA102** (protocol-compatible, standard pairing).
  Global brightness on the unit is a 6-bit master dimmer (the SK9822 current control) —
  the hardware ceiling; MANIFOLD content stays full-range and never fights it.
- Channel math: 120 px × 3 ch = 360 ch → **one universe per strip** (`PerUniverse`
  addressing, already the default). 8 strips = 8 universes.
- **UDP side-channel (port 8216, `0xA8` opcodes):** switch SD file, set global
  brightness/RGBW (0–63), query program — separate from Art-Net. `brightness=0` is a
  **hardware blackout** that works even if the Art-Net path is what broke; wire it as the
  understudy's second blackout rung (D8). SD standalone playback (64 DAT files) doubles as
  a computer-dead fallback. **Verify at rig bring-up:** what the unit does when the
  Art-Net stream stops (hold last frame / SD fallback / dark) — this decides how critical
  the dying-breath blackout is. The unit's Net2 frame-rate test menu can confirm
  MANIFOLD's delivered fps from the LCD.

## 3. Decisions

- **D1 — Strips are an accent instrument, not a picture.** Percussion, not video: mostly
  dark, hits at full blast. Duty cycle is the balance knob — the performer controls it by
  what they trigger, not by a dimmer curve. The stage-sampling edge-extend look survives
  only as one *clip choice* (D4), never as the system.
- **D2 — The strip array is one small 2D canvas.** Strips become a tiny **island** in the
  multi-display model (e.g. 8×120: columns = strips, rows = LEDs bottom→top). Layers target
  it via the existing layer domains; **patterns are ordinary 2D content** (Peter: "2D
  patterns") — a chase sweeps across columns, a pulse runs up all strips, a strobe fills the
  island. No 1D special case anywhere.
- **D3 — Patterns = generators + trigger clips. Nothing new.** A pattern is a JSON generator
  preset rendered at island resolution, placed in a trigger clip, fired from a pad/cue like
  everything else. Ship a bundled **LED preset pack** (chase, scan, pulse, strobe, sparkle,
  fill) tuned for tiny resolutions. Compose from existing primitives; if an atom seems
  missing at implementation, the section 2.5 audit rule applies (expect none — these are gradients,
  steps, and noise).
- **D4 — Edge-extend becomes a clip.** The current always-on stage sampling turns into an
  "ambient" generator choice the performer can place like any clip. Same shader, demoted
  from architecture to content.
- **D5 — Fixture patch replaces rig constants.** Per-fixture config, UI-editable, persisted
  in the **venue profile** (same display-identity-keyed store as multi-display #13 /
  projection mapping): name, pixel count, color order (enum: RGB/BGR/GRB/BRG/…/RGBW),
  universe + start channel, island column/region, `reversed` flag (strips wired top-down).
  One-time migration from `LedSettings`; Peter's rig becomes the bundled example patch.
- **D6 — Two output protocols: Art-Net and sACN (E1.31).** `LedOutputDef` = `ArtNet { ip,
  port }` | `Sacn { priority, multicast }`. sACN is what lighting consoles and larger rigs
  expect; universes are 0-based in Art-Net, 1-based in sACN — the patch UI shows the
  protocol's native numbering, storage is internal-canonical. Multiple outputs allowed
  (fixtures reference an output by id).
- **D7 — Send path keeps its shape.** Island texture → async readback (existing) → per-
  fixture channel pack (color order + reversal applied here) → UDP. Content-frame cadence,
  dirty-gated, pre-allocated buffers. `blur_radius` survives as a per-fixture-patch option.
- **D8 — Gig resilience owns LED blackout.** `blackout()` already exists; the understudy /
  panic path (GIG_RESILIENCE_DESIGN) must fire it — dead render must never freeze strips at
  full white. Cross-reference added there at implementation.

## 4. Data model (sketch)

```rust
// venue profile (not the project — rig config travels with the venue)
pub struct LedPatch {
    pub outputs: Vec<LedOutputDef>,     // id + ArtNet{ip,port} | Sacn{priority,..}
    pub fixtures: Vec<LedFixture>,
}
pub struct LedFixture {
    pub name: String,                   // "stage-left 1"
    pub output: LedOutputId,
    pub pixels: u32,                    // 120
    pub color_order: ColorOrder,        // Bgr for SK9822 via H807SA (today's is_bgr)
    pub universe: u16,
    pub start_channel: u16,
    pub island_column: u32,             // which column of the strip island
    pub reversed: bool,                 // wired top-down
}
```

Project side: the strip island is declared like any island (multi-display model); the patch
maps island columns → wire. Project ↔ venue separation matches projection mapping: content
addresses the island, the venue profile knows the copper.

## 5. Phasing

- **P1 — Patch generalization + sACN.** `LedPatch`/`LedFixture`/`LedOutputDef` replace
  `LedSettings`; sACN sender alongside Art-Net; per-fixture pack (color order, reversal,
  start channel); migration from old settings; patch UI (list + fields, no canvas). Works
  against today's edge-extend source — **no island dependency**. Gate: focused
  `manifold-led` tests (packet bytes vs known-good captures for both protocols) + live rig
  smoke test.
- **P2 — Strip island.** Register the strip array as an island (needs multi-display P1–P3);
  edge-extend becomes a clip choice (D4); 1:1 column mapping through the patch. Gate: a
  generator on the strip island lights the physical strips correctly oriented.
- **P3 — LED preset pack + performance wiring.** Bundled presets (D3), trigger-clip
  examples, cue names in the venue profile. Acceptance: **fire a chase from a MIDI pad on
  the real rig with zero code edits** — patched entirely through UI.

## 5a. UX home addendum (2026-07-06, Peter-ruled)

The fixture patch surface lives on the unified Stage surface (MULTI_DISPLAY section 5a):
fixtures are objects on the same venue canvas as displays and projectors. Selecting
a fixture shows patch summary + test controls in the side flap; deep patch detail
(universe/channel routing, per-fixture test patterns) opens as the focused per-object
mode with breadcrumb back. The Project Settings ▸ LED page stays a summary + entry
link (APP_SHELL section 6.2). Data model, protocols, and phasing below are untouched.

## 5b. MVP — LED layer type + direct-drive patterns (2026-09-03, k3 lead + Peter)

Peter's directives (2026-09-03, verbatim — these decide the MVP):

- "let's keep it simple for now, working MVP that can extend and be built at with
  features and changes as needed"
- "yes LED switches over to the DMX layer outputs, no blend so you can trigger the
  lights without 'background' stuff making it look weird"
- "The MVP plan must let me create a new LED layer type, works with groups, standard
  layer functions. It should default place this new LED generator graph as it's
  generator preset so I can use these new outputs straight away with our standard
  workflow and UI and UX."

### 5b.1 Audit (verified 2026-09-03; extends the section 1 snapshot — re-verify anchors per phase)

- **Layer kind is an extensible enum.** `LayerType { Video = 0, Generator = 1, Group = 2,
  Audio = 3 }` at `crates/manifold-core/src/types.rs:116`, manual serde (int + string,
  unknown → `Video` fallback, `:135-154`). LED routing today is only the `blit_to_led`
  flag (`crates/manifold-core/src/layer.rs:73`).
- **Generator render sizing is global — no per-layer size exists.** `GeneratorRenderer`
  holds global `width/height` (`crates/manifold-renderer/src/generator_renderer.rs:143-144`);
  `resize_gpu` resizes all render targets at one `render_w × render_h`
  (`crates/manifold-renderer/src/generator_renderer.rs:867-884`, called
  `crates/manifold-app/src/content_pipeline.rs:3281`). Layer compositing into main-sized
  `layer_bufs`: `generate_layers` (`crates/manifold-renderer/src/layer_compositor.rs:1705`,
  main dims `:1707-1708`).
- **LED-res (8×120) compositing machinery already exists.** `led_main` PingPong at
  `frame.led_composite_size` (`layer_compositor.rs:504-512`, ensured `:2064-2073`);
  per-group `led_group_bufs` (`:526`, ensured/resized `:802-825`); LED group FX contexts
  at LED res (`:1386-1404`). Grid size source: `ContentPipeline.led_grid_size` ←
  `LedSettings{strip_count, leds_per_strip}` (`crates/manifold-app/src/content_pipeline.rs:1017,1239`,
  set via `InitLedOutput` `crates/manifold-app/src/content_commands.rs:1293-1299`).
- **The screen-res → LED-res blend exists.** `blend_layers_to_led`
  (`layer_compositor.rs:2051`) blends top-level L-flagged layers' full screen-res output
  into LED-res `led_main` (`:2314-2333`, Normal + opacity); L children of groups fold into
  `led_group_bufs` (`:2189-2214`); non-L children substitute 1×1 black
  (`:2079-2104`); runs before `fold_groups`, children route by own flag (`:2047-2048`).
- **Send path is untouched by the MVP.** `led_source_texture()` (`content_pipeline.rs:3682`)
  → `content_thread.rs:879-913` → `LedOutputController::process_frame` → edge-extend
  (`crates/manifold-led/src/blit.rs:13`) → readback → DMX (`crates/manifold-led/src/dmx.rs`).
  **Erratum (2026-09-03, found at execution time by the MVP-P1 pre-flight):** the
  edge-extend pass is identity unconditionally at HEAD — `artnet.rs:78-81` documents that
  `LedSettings` edge widths are intentionally ignored and `artnet.rs:183` hardcodes
  `blit.blit(enc, source, 0.5, 0.5, BLUR, led_gain)` (widths 0.5 = identity in U per the
  shader math at `led_edge_extend_compute.wgsl:38-47`). The settings edge-width fields are
  dead config (no reader outside serde defaults). A vertical blur (radius 1.5) +
  `led_gain` + chroma-preserving clip still apply. Consequence: direct-drive content is
  NOT mangled by edge cropping — D13's requirement is already satisfied by existing code.
  BUG-6pmq (LED blend dispatch sized by compositor dims) stays open — masked in production
  because the fullscreen compositor ≥ the grid; the MVP does not change this.
- **Layer creation with a default generator preset is a solved pattern.** Generator
  layers default to PLASMA at `crates/manifold-app/src/ui_bridge/editing.rs:245-256` via
  `AddLayerCommand` (`crates/manifold-editing/src/commands/layer.rs:13,47`).
- **Presets are disk JSON, hot-reloadable.** Bundle root
  `crates/manifold-renderer/assets/generator-presets/` (32 presets; loader
  `crates/manifold-renderer/src/preset_loader.rs:162-168`). Type id = filename stem
  (`node_graph/bundled_presets.rs:23-24`). Picker: `build_preset_picker_items`
  (`crates/manifold-app/src/ui_root/dropdowns.rs:113-161`).
- **MIDI pad → clip triggering is existing machinery.** `midi_input.rs` → `ClipLauncher`
  → `live_clip_manager.rs` (phantom clips, 5ms guard); layer matching via
  `Layer.midi_note/channel/device/trigger_mode` (`crates/manifold-core/src/layer.rs:146-155`).
  LED layers inherit all of it — zero new trigger work.
- **Pattern atoms exist.** 254 primitives (`rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/`);
  beat gates/ramps, directional ramps, trigger cycling, texture combines all present.
  Per D3's prediction: the pack composes from existing atoms, no new primitives expected.
  Section 2.5 audit re-run at preset-authoring time regardless.

### 5b.2 Decisions

- **D9 — MVP routing reuses the existing downsample blend; LED layers render at main res
  like every layer and blend into the 8×120 `led_main` through `blend_layers_to_led`.
  No per-layer render sizing in the MVP.** Generators are UV-space; a pattern rendered at
  main res and sampled down to 8×120 is visually equivalent to native 8×120 rendering for
  pattern content (supersampled, marginally softer on hard edges). Per-layer sizing is new
  machinery in `resize_gpu`/`GeneratorRenderer`/`layer_bufs` for a perf problem the MVP
  doesn't have (one LED layer ≈ one extra full-res generator, well inside frame budget).
  Rejected: native LED-res rendering in the MVP, because it makes P1 a render-pipeline
  refactor instead of a routing change. Consequences, stated honestly: hard-edged patterns
  soften slightly at downsample; an abused LED layer (heavy 3D preset) costs a full-res
  render. Both are accepted for the MVP; the trigger that revokes this decision is in
  Deferred.
- **D10 — Switch, no blend (Peter's call).** When at least one LED-type layer has an active
  clip this frame, the LED composite carries LED-type layers ONLY; `blit_to_led` mirror
  layers contribute nothing. Otherwise the mirror path runs as today; neither → blackout.
  Edge, stated: an active LED-layer clip at opacity 0 switches the strips to black rather
  than falling back to mirror — predictable ("what you trigger is what you get"), and it
  keeps the switch rule to one boolean: *active LED clips, yes/no*. **Mixed groups:** when
  the route is direct, mirror children inside a group are skipped ENTIRELY — no content,
  and no black-block either (the group fold's non-L-child stand-in
  `layer_compositor.rs:2079-2104` must not run for them). "Skip" is the only reading
  consistent with Peter's "no blend … without background stuff making it look weird", and
  it applies recursively through nested groups.
- **D11 — LED layer = new `LayerType::Led`; the render path carries a route enum, not the
  raw flag.** Model side, `Layer.blit_to_led: bool` keeps its persisted mirror meaning and
  is inert on LED-type layers. Render side, `LayerOutput.blit_to_led: bool`
  (`layer_compositor.rs:377`) is replaced by `enum LedRoute { None, Mirror, Direct }`,
  constructed ONCE at descriptor build (`:1851`, `:1958`): `layer_type == Led → Direct`,
  else `blit_to_led → Mirror`, else `None`. Every downstream site (`:1245,:1323,:1441`
  allocation/warmup scans, `:2057,:2122,:2137,:2189,:2307,:2475` fold paths,
  `content_pipeline.rs:168,179` render-skip filters) reads the enum — no site combines the
  two raw predicates with `||`, and no site re-derives the route. Standard section 6
  applies: compiler-driven migration (replace the field, let red builds enumerate sites),
  deletion gate in P1.
  Standard layer machinery applies unchanged to LED layers: timeline clips, opacity,
  blend, effect chains, groups (an LED child routes through the existing
  `blend_layers_to_led` group fold), MIDI pad triggers.
- **D12 — New LED layers default to the bundled LED preset (Peter's call).** Layer
  creation sets `gen_params` to the pack's neutral preset (LED Fill) so the standard clip
  workflow — draw a clip, it plays the generator, trigger from a pad — works with zero
  setup. Precedent: Generator → PLASMA default at `ui_bridge/editing.rs:245-256`.
- **D13 — The send path is untouched, and identity mapping is already unconditional.**
  LED-layer composites flow through the existing `led_main` → LED master FX →
  edge-extend/gain → readback → ArtNet chain. Erratum (2026-09-03, execution-time
  pre-flight finding): the controller already forces edge-extend to identity for ALL
  routes (`artnet.rs:183`, widths 0.5/0.5; settings widths deliberately dead) — so the
  direct route needs NO width plumbing and no per-route numbers anywhere. The original
  worry (edge-crop mangling direct-drive content) is moot at HEAD. No new code in
  `manifold-led`; no width-selection call site in the app crate. Vertical blur +
  `led_gain` apply to both routes unchanged.
- **D14 — LED layers are screen-invisible (Peter's demo: "screen content unaffected").**
  A Direct-route layer never blends into the main screen composite. Mechanism: the
  existing occluded-layer blend-skip path (`frame.occluded_layers` consumed at
  `blend_layers`) — the descriptor build marks Direct-route layers occluded for the screen
  pass; no new flag, no new blend branch. Corollary, stated so no worker re-derives it:
  the render-skip filters (`content_pipeline.rs:168,179`) must treat a Direct-route layer
  as render-needed whenever it has active clips — screen-invisible ≠ render-skippable,
  or the LED composite goes black exactly when the layer is hidden, i.e. always.

**Plausible-wrong architectures, forbidden by name:** (1) You will want a bespoke 1D
pattern engine or per-LED pattern language — no: patterns are ordinary generator presets
(D3), the graph is underneath. (2) You will want to render LED layers natively at 8×120 in
the MVP — no: D9; that refactor is deferred, not skipped. (3) You will want to composite
mirror + LED routes with a blend/crossfade — no: D10, switch only; a fader between routes
is exactly the "background stuff making it look weird" Peter rejected. (4) You will want
to OR `blit_to_led` with `layer_type == Led` at each render site — no: D11, one route enum
built once at descriptor time. (5) You will want to add an "identity mode" to the
edge-extend shader or branch the controller — no: D13, identity is already forced
  unconditionally at the controller (`artnet.rs:183`); there is nothing to pick. (6) You will want
a new "screen hidden" flag for LED layers — no: D14, the occluded-layer blend-skip path
already exists.

**Forward-compat cost, stated honestly:** `LayerType` serde falls back unknown values to
`Video` (`types.rs:135-154`), so a pre-MVP build loading a project with LED layers
silently downgrades them to video layers (tiny content on screen, no strip output).
Accepted — same hazard class as when `Audio = 3` was added.

### 5b.3 Data model seam

```rust
// crates/manifold-core/src/types.rs:116 — extend, don't wrap
pub enum LayerType {
    Video = 0,
    Generator = 1,
    Group = 2,
    Audio = 3,
    Led = 4,   // serde: int 4 + string "Led"; unknown → Video (existing fallback)
}

// crates/manifold-renderer/src/layer_compositor.rs:377 — replaces LayerOutput.blit_to_led
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedRoute { None, Mirror, Direct }
// Built once at descriptor construction (:1851, :1958):
//   layer_type == Led → Direct
//   else blit_to_led  → Mirror
//   else              → None
```

Default generator for a new LED layer: `PresetTypeId("LED Fill")` (bundled by MVP-P2;
MVP-P1 may ship a minimal single-node Fill placeholder preset under the same id — the id is
the contract, the graph grows underneath). Serialization of the layer is unchanged
(`Layer` already carries `layer_type` + `gen_params` + `blit_to_led`); round-trip comes
free. The `blit_to_led` model field is untouched (persisted mirror flag, UI toggle hidden
for LED-type layers per NIT below).

### 5b.4 Phasing (one phase = one session)

**MVP-P1 — LED layer type + switch routing + vertical slice.**

- *Entry state:* main checkout at `origin/main`. Re-verify anchors:
  `rg -n "enum LayerType" crates/manifold-core/src/types.rs`,
  `rg -n "blend_layers_to_led" crates/manifold-renderer/src/layer_compositor.rs`,
  `rg -n "blit_to_led" crates/manifold-core/src/layer.rs`,
  `rg -n "PLASMA" crates/manifold-app/src/ui_bridge/editing.rs`. A moved/missing anchor is
  an escalation, not a guess.
- *Read-back (first step):* restate D9–D14, the six forbidden architectures, and the
  entry-state results. Then read `types.rs:116-154`, `layer_compositor.rs:2040-2340`,
  `ui_bridge/editing.rs:240-270`, `commands/layer.rs:1-80`, and the
  `LedRoute` seam brief below.
- *Seam brief — `blit_to_led: bool` → `LedRoute` (standard section 6):* old → new written
  out above (5b.3). Logic call-site inventory (re-derive at execution with
  `rg -n "blit_to_led" crates/`, `rg` returns additional comment/test/doc hits — the
  logic sites are): `content_pipeline.rs:168,179,2375`; `layer_compositor.rs:377 (field
  decl),1245,1323,1441,1851,1958,2048,2057,2122,2137,2189,2307,2475,2516`. If a logic
  site appears that is not in this list, stop and list it before touching anything.
  Compiler-driven migration: rename the `LayerOutput` field first, let red builds
  enumerate the sites. Model field `Layer.blit_to_led` is NOT migrated (stays, per D11).
- *Deliverables:* (1) `LayerType::Led` variant + serde int/string
  (`crates/manifold-core/src/types.rs`); (2) layer-creation UI entry "LED" with default
  `gen_params` = LED Fill preset, shaped like the Generator/PLASMA path
  (`ui_bridge/editing.rs`, `commands/layer.rs`); (3) `LedRoute` migration per the seam
  brief — descriptor build at `:1851,:1958` is the ONLY site that reads
  `layer_type`/`blit_to_led` together; (4) routing: `blend_layers_to_led` folds
  `Direct`-route layers exactly like today's Mirror layers (top-level and group-child
  paths), with the D10 partition — active Direct-route clips ⇒ Mirror-route layers
  excluded, and under the direct route mirror children in mixed groups are skipped
  entirely (no content, no black-block, recursive through nested groups); (5) the route
  boolean comes from frame state (layer present in the frame's active-clip set), NOT from
  the project-state scans — those three scans (`:1245,:1323,:1441`, allocation/warmup)
  switch to `route != None` so LED-type layers get warm buffers (BUG-037 (glp-first-render-stall) class if missed); (6) D14: descriptor build marks Direct-route
  layers occluded for the screen pass; render-skip filters (`content_pipeline.rs:168,179`)
  treat Direct layers as render-needed when clip-active; (7) VACATED by erratum — identity
  edge-mapping is already unconditional at the controller (`artnet.rs:183`), no
  width-selection work exists; (8) UI:
  hide the `blit_to_led` toggle for LED-type layers (`ui_bridge/layer.rs:135-147`);
  LED-type lanes reuse the existing `is_led` treatment (`ui_bridge/projection/timeline.rs:69`)
  — no new styling; (9) tests below.
- *Gate (positive):* pixel tests hooked at `led_composite_texture()`
  (`layer_compositor.rs:3145` — pre-controller; LED master FX disabled in fixture so the
  texture is FX-independent; precedent: the BUG-2ptv (LED output dead) composite pixel
  tests): LED Fill layer active ⇒ all 8×120 cells ≈ fill colour; a mirror layer + an
  active LED layer ⇒ LED composite carries only LED content; a mixed group under the
  direct route ⇒ mirror child's contribution is byte-identical to it being absent; no
  active LED or mirror layer ⇒ blackout; LED layer inside a group ⇒ same through the
  group fold; screen-invisibility: with an active LED layer, the main screen composite is
  pixel-identical to the LED layer not existing. Edge-identity pinning: an app-crate test
  drives `EdgeExtendBlit` (public API) at widths (0.5, 0.5) and asserts output == input at
  8×120 (pins the identity behavior the controller already forces; `manifold-led` itself
  stays untouched). Round-trip: save → reload → LED layer
  keeps type + preset, and a clip on it still drives the LED composite *after* reload.
  L3: a `scripts/ui-flows/` flow creates an LED layer via the real UI path and asserts it
  appears with the LED lane treatment.
- *Gate (negative):* `rg -n "blit_to_led" crates/manifold-renderer/src/layer_compositor.rs`
  returns hits ONLY at the descriptor-construction lines (`:1851`,`:1958` field builds) —
  every other render-path site reads `LedRoute`; `rg -n "blit_to_led \\|\\||layer_type == LayerType::Led \\|\\|"`
  returns zero hits anywhere (no OR-ed predicates); `rg -n "blit_to_led\|LedRoute\|left_edge_width"
  crates/manifold-led/src/artnet.rs crates/manifold-app/src/content_thread.rs` shows no
  behavioral change (D13 — no width plumbing; identity already forced at the controller);
  no `#[ignore]`, no new `Arc<Mutex>`.
- *Acceptance demo:* L3 flow artifact + headless PNG of the LED grid for Peter to look at;
  click-script for the rig (L4, Peter): (1) launch main, enable LED in settings, (2) create
  LED layer, draw a 1-bar clip, (3) play — strips show fill, screen content unaffected,
  (4) delete clip — strips return to mirror/blackout.
- *Performer gesture:* trigger a clip on the LED layer from a MIDI pad mid-set; the strips
  show the pattern and nothing else, screen unaffected. Gate exercises the gesture via the
  existing phantom-clip path.
- *Forbidden moves:* per-LED-res render targets (D9); blending the two routes or
  black-blocking mirror children in mixed groups (D10); adding machinery to the
  `manifold-led` send path, including any width-selection call site — identity is already
  forced at the controller, adding plumbing is scope creep (D13); OR-ing the route
  predicates at any site (D11); a new
  screen-visibility flag instead of the occluded path (D14); changing `blit_to_led`
  mirror semantics; TODO-as-deferral on the switch partition.
- *Test scope:* `cargo nextest run -p manifold-core -p manifold-renderer -p manifold-app`
  (touched crates); gpu-proofs gate if compositor dispatch is touched (it is —
  `scripts/gpu_proofs_gate.py`); clippy `-p` same set.

**MVP-P2 — LED preset pack (Fill, Chase, Scan, Pulse, Strobe).**

- *Entry state:* MVP-P1 landed. Anchors: `ls crates/manifold-renderer/assets/generator-presets/`,
  `rg -n "GENERATOR_CATALOG" crates/manifold-renderer/src/preset_loader.rs`.
- *Read-back:* restate D3/D9/D12; read one bundled preset end-to-end (BasicShapes.json) and
  the beat-gate/ramp primitives it can compose from.
- *Deliverables:* five generator preset JSONs, `LED ` name-prefix, authored UV-space so
  they work at any grid dims; params: colour, speed (beats), direction, tail/duty where
  applicable; beat sync composed from existing beat-ramp/gate atoms. Pre-flight every
  preset through `graph-tool validate --kind generator` and `graph-tool fusion`
  (`docs/GRAPH_TOOLING_DESIGN.md`). Section 2.5 audit recorded per preset: exists / one
  wire away / genuinely new (expect all "exists").
- *Gate (positive):* LED composite at 8×120 (via `led_composite_texture()` — under D9
  nothing renders natively at LED res, and the gate must not imply such a harness):
  Chase position advances monotonically with
  beat phase; Strobe flips between two states at the duty point; Fill/Scan/Pulse pixel
  patterns match CPU-computed expected output (value tests, not looks). Default-preset
  contract: creating an LED layer loads `LED Fill` with no missing-id fallback.
- *Gate (negative):* `rg -n "create_compute_pipeline\\(include_str" crates/manifold-renderer/src/node_graph/primitives/`
  unchanged (no bespoke kernels smuggled in as presets); zero new primitive files.
- *Acceptance demo:* L2 headless PNG strip of the five patterns at 8×120 for Peter;
  click-script (L4, Peter): pad-fire each preset on the real rig.
- *Performer gesture:* fire Chase from a pad; it runs at the track BPM, one LED-width comet
  sweeping all strips, and stops clean on clip end.
- *Forbidden moves:* new primitives without the 2.5 audit; presets authored at fixed
  pixel counts instead of UV space; shipping the pack without the fusion check.
- *Test scope:* renderer crate tests + graph-tool pre-flight; gpu-proofs if fusion path
  touched.

### 5b.5 Deferred (explicitly not MVP — each with its revival trigger)

- **Native LED-res rendering** (per-layer sizing in `GeneratorRenderer`/`resize_gpu`):
  revive when a profiler run shows an LED layer's full-res render costing frames, or when
  hard-edge pattern fidelity is missed on the rig.
- **sACN output** (original P1/D6): revive when a console or non-Art-Net rig is in the room.
- **Venue-profile fixture patch + Stage-surface fixture UI** (original P1/D5/5a): revive
  when the rig changes or per-strip reversal/colour-order is needed live.
- **Strip island via multi-display** (original P2/D2 as island): revive when
  MULTI_DISPLAY P2+ lands; absorbs the MVP's standalone LED target.
- **Edge-extend demoted to a clip choice** (D4): revive with the island phase — in the MVP
  the mirror path stays as-is.
- **LED grid preview panel in the UI:** revive when Peter can't tell what the strips show
  without looking at the rig.

## 6. Decided — do not reopen

1. Strips = accent instrument; stage pass-through as the system is rejected (gimmick).
   Edge-extend survives only as a clip choice.
2. Strip array = one 2D island; patterns are ordinary 2D generators — no 1D pattern engine,
   no per-strip special case.
3. Patterns fire as trigger clips through existing infrastructure; no new trigger machinery.
4. Patch (fixtures + outputs) lives in the venue profile, UI-editable; rig constants and
   `LedSettings` are migrated away.
5. Art-Net **and** sACN; per-fixture color-order enum replaces `is_bgr`; controller
   internals (H807SA ports) never enter the data model.
6. Send path: async readback → pack → UDP at content cadence, dirty-gated, no per-frame
   allocation. Blackout is wired into the gig-resilience panic/understudy path.
7. (MVP, 2026-09-03) MVP routing = existing downsample blend, no native LED-res rendering
   (D9); switch-not-blend between LED route and mirror route, mirror children in mixed
   groups skipped entirely (D10); LED layer = new `LayerType::Led`, render path carries a
   `LedRoute` enum built once at descriptor time, model `blit_to_led` untouched (D11);
   new LED layers default to the bundled LED Fill preset (D12); send-path machinery
   untouched — identity edge-mapping already forced at the controller for all routes,
   no per-route plumbing (D13); LED layers are screen-invisible via the existing occluded-layer path (D14).
8. (MVP, 2026-09-03) sACN, venue-profile patch, strip island, edge-extend demotion, and
   LED grid UI preview are deferred with named revival triggers (section 5b.5) — not
   forgotten, not in scope.
