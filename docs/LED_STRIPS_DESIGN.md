# LED Strips — Play the Strips + Generalized Patch

**Status: IN PROGRESS — MVP-P1 + MVP-P2 SHIPPED 2026-09-03 (LED layer type, nine-pattern
pack); MVP-P3 SHIPPED 2026-09-04 (guard fixes; Dmx rename; LED browser category + scoped
open); MVP-P4 SHIPPED 2026-09-05 (LED composite preview band on the DMX lane card). Owed:
Peter calls (scene-panel, drag-convert, MIDI import — beads); Vec4 color binding;
patch/sACN/island phases (deferred, section 5b.5).**
**Prerequisites: none for the MVP (LED-resolution compositing machinery already exists — see MVP audit).
Original P2 (strip island) rides the island model from `docs/MULTI_DISPLAY_DESIGN.md` P1–P3.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 8 (Execution protocol)
before any phase. Section 1 is a 2026-07-03 snapshot — re-verify `manifold-led` anchors per the
section 8.3 pre-flight before each phase.**

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

**MVP-P3 decisions (2026-09-04, four-lane read-only audit; bead BUG-ng6a (DMX lane first-class campaign)):**

- **D15 — `LayerType::Led` renames to `LayerType::Dmx` (Peter's call, verbatim: "the
  LayerType should be updated to DMX … the easiest time we will ever have to refactor base
  stuff like this. … A first class 'DMX' type sounds sensible here?").** Discriminant stays
  `4`; Deserialize accepts int 4 + string "Dmx" + string "Led"; Serialize keeps emitting
  the int unconditionally (serialization audit: no code ever writes the string form, so
  **no migration rung** — precedent `Audio = 3`, `migrate.rs:678-694` bidirectional
  deserialize). Rejected: keep `Led` (Peter's directive above). Rejected: "Universe"
  (Resolume's Lumiverse concept) — a lane is the DMX *source* whose pixels map onto
  fixtures across universes; it is not itself a 512-channel universe. DMX names the data
  (DMX512), not one wire protocol — stays true when sACN lands, since sACN is a transport
  for DMX512.
- **D16 — Every generator-mutation guard widens from `== LayerType::Generator` to one
  "gen-carrying layer" predicate.** Audited sites: `change_generator_type`
  (`layer.rs:884`), `restore_generator_state` (`:945`), load-time identity reconcile
  (`:913`), `reset_all_effectives` (modulation.rs:204), drag-move
  (`editing_host.rs:170`), scene panel (`projection/inspector.rs:183`), MIDI import
  (`midi_import.rs:67`), percussion import (`percussion_import.rs:372`), string-param
  collect (`io/collect.rs:142`). The `change_generator_type` guard is the root cause of the
  observed card jank — the picker is a silent no-op on LED lanes (BUG-ev8u (generator picker no-ops on LED/DMX lanes)); the `reset_all_effectives` guard makes envelopes ratchet
  on LED lanes (BUG-p3rq (modulation reset gated to Generator lanes)). **Rejected: adding
  `|| is_dmx()` per guard** — that manufactures N copies of one distinction; the predicate
  lives once on `Layer` and every gate reads it. Where a site's intent genuinely differs
  (drag-move LED↔Video, scene-panel scope, MIDI import meaning) the item is a Peter call,
  not a predicate widening — those are beads, not silent edits. **The predicate is type
  membership, not gen_params presence:** `Layer::hosts_generator() = matches!(self.layer_type,
  LayerType::Generator | LayerType::Led)`. A presence-based guard is a shipped regression —
  `change_generator_type` and `restore_generator_state` create gen_params via
  `get_or_insert_with` when absent (layer.rs:890, :951) precisely to serve Generator layers
  with no generator yet; gating on presence would turn "assign a generator to an empty
  Generator layer" into a silent no-op (adversarial review, k3, 2026-09-04).
- **D17 — DMX-ness in the UI is data on shared surfaces, never a new surface.** The card
  stays the generic manifest-backed `ParamCardPanel` (WIDGET_TREE_DESIGN section 5b is the
  machine rule); the browser stays the one `BrowserPopupPanel`. What arrives: presets
  re-tag `category: "LED"` (from "Pattern"), the registry stops discarding generator
  categories (`preset_type_registry.rs:111`), generator mode renders the existing category
  chip row, and opening the browser from a DMX lane scopes to "LED" via the existing
  `set_category` hook (`browser_popup.rs:323`). **Forbidden: a bespoke DMX card, a DMX
  browser, bespoke param rows — any of those forks the unified surface for zero capability.**
- **D18 — Preset ids and filenames keep the `LED *` names.** Bundled preset ids are JSON
  filename stems (`bundled_presets.rs:23`) referenced by saved projects; renaming them is a
  compat problem for zero behavior gain. The category carries the grouping. Consequences,
  stated honestly: preset display names keep the "LED" prefix while the lane type says DMX
  — accepted; the presets *are* LED-pattern presets.
- **D19 — The card reconcile path was never the bug; the model no-op was.** Audited
  end-to-end: `configure_gen_params` panel reuse keyed on layer id is intentional
  (`inspector/render.rs:191-213`); `ParamCardPanel::configure` fully rebuilds rows/state
  from the new surface; the projection gate at `projection/inspector.rs:1138-1156` is keyed
  on `gen_params` presence, not layer type. Recorded so no worker re-chases the reconcile
  path (both audit lanes initially suspected it; the defect is one guard at `layer.rs:884`).
- **D20 — Scene Setup scope for DMX lanes is a Peter call, deferred** — BUG-p8ma (decide Scene Setup panel scope for DMX lanes). The panel's `!= Generator` guard would work
  widened (DMX lanes have gen_params + bundled defs), but whether a DMX lane *should* host
  scene setup is product judgment, not an audit finding.
- **D21 — Muted/invisible DMX lanes still rendering full-res generators is deferred** —
  BUG-n6dm (skip generator GPU work for muted/invisible LED lanes). Overlaps the 5b.5
  native LED-res rendering revival trigger — do not build the skip and then rebuild it
  natively.
- **D22 — The inspector preview shows the whole LED composite, not the selected layer's
  output (Peter's ask, 2026-09-04: "a small preview blit to the LED layer inspector …
  useful as a debug tool and preview screen").** The composite is the truth of what the
  strips display: under D10's switch, a per-layer view would show content the strips are
  not playing — the exact confusion a debug preview exists to prevent. Rejected: per-layer
  sampling (extra compositor plumbing, misleading during the switch). This is the 5b.5
  "in-app LED grid preview" revival, scoped to the DMX lane inspector; a standalone panel
  stays deferred (5b.5 trigger unchanged for multi-surface needs).
- **D23 — Data path: the latest completed Art-Net readback rides ContentState to the UI;
  no new channel, no shared state.** The send path already readbacks the 8×120 strip
  texture every frame (`manifold-led/src/readback.rs` `try_read` → `Vec<u8>`); the content
  thread caches the most recent completed readback and publishes it on the existing
  content→UI snapshot (a few KB per frame, drained to latest — the channel already
  coalesces). The UI uploads it to a small texture once per new frame. Rejected: a second
  readback for the UI (doubles GPU staging for bytes that already exist); `Arc<Mutex>`
  handoff (banned — snapshots are the house pattern); UI-side re-render of the generator
  (drift — the preview must be the send path's own pixels, not a parallel render).
  **Ordering, stated honestly (adversarial review 2026-09-04): the readback is
  post-`led_gain` (GPU, `blit.rs:27`) but pre-master-brightness (CPU, `dmx.rs:105` in
  `pack_and_send`) — the preview tracks pattern and gain, NOT the LED brightness slider.**
  **Payload shape: `try_read` returns `Arc<[u8]>`** (`readback.rs:97` already allocates
  this buffer per completed frame; the same allocation serves `pack_and_send` and the
  snapshot — no clone, no new per-frame alloc).
- **D24 — Presentation: faithful-orientation band (native 8×120, 8 vertical strips) at the top of the DMX lane's
  generator card, drawn through the viewport bitmap path; black when the strips are black.**
  The texture is 8×120 (strips × LEDs). ~~Displayed faithfully it is a 3px sliver at card
  width — useless as a debug view. Transposed, each row is one strip, read left-to-right
  like the rig.~~ (Superseded 2026-09-05 — see the addendum below: faithful orientation
  shipped, centred vertical strip.) No label, no placeholder text; idle states are drawn truthfully (below).
  **Machinery: the viewport bitmap path (`panels/viewport/render.rs`:34,197,316 —
  dirty-flagged per-frame CPU bitmap → GPU upload → blit via the layer-bitmap path,
  `viewport.rs:1474`) — browser thumbnails are static atlas registrations, NOT the
  precedent.** **Payload is a state enum, not `Option<Vec<u8>>`: `Frame(pixels)` /
  `Black` / `None`** (adversarial review 2026-09-04): `None` (no LED controller or never
  enabled — no data exists) renders NO band; `Black` (the send path's blackout or disabled
  state, `controller.rs:94-100/:88/:120`) renders a black band AND clears the cached
  frame — a stale chase must never animate in the preview while the rig is dark; `Frame`
  with black pixels renders a black band naturally. **Orientation is pinned empirically
  against the `led_composite_pixel_tests` known strip/LED mappings** (texture x = strip
  index, y = LED position; `blit.rs:32,52-53`) — transpose plus both flips (LED-0
  left/right, strip-0 top/bottom) written into the test; guessing ships a preview that
  mirrors the rig. Rejected: faithful-orientation sliver (illegible); placeholder copy in
  idle (a debug tool must show state, not reassure).
  **Addendum (owner feedback 2026-09-05): the transpose is rejected — the band ships
  faithful physical orientation: 8 vertical strips as a native 8×120 bitmap (direct copy,
  v-flip only so LED 0 sits at the bottom), integer-scaled, height ≤240px, width at the
  1:15 strip ratio, centred at the card top. The "illegible sliver" rejection above is
  superseded; a vertical strip at 2× scale reads fine and matches the rig.**

**MVP-P3 plausible-wrong architectures, forbidden by name:** (1) You will want a bespoke
DMX card or DMX browser — no: D17. (2) You will want to rename the `LED *.json` preset ids
to match the DMX lane — no: D18, ids are a persisted contract. (3) You will want to fix each
`== Generator` guard with an `|| is_dmx()` — no: D16, one predicate. (4) You will want to
touch the card reconcile/reuse machinery for the swap jank — no: D19, the model guard is
the whole defect. (5) You will want to widen Scene Setup or MIDI import silently — no: D16,
those are Peter calls, carried as beads. (6) You will want a migration rung rewriting layer
type 4 — no: D15, the int is the only wire form and it stays 4.

### 5b.3 Data model seam

```rust
// crates/manifold-core/src/types.rs:116 — extend, don't wrap
pub enum LayerType {
    Video = 0,
    Generator = 1,
    Group = 2,
    Audio = 3,
    Dmx = 4,   // serde: int 4 + strings "Dmx"|"Led"; unknown → Video (existing fallback)
}

// crates/manifold-renderer/src/layer_compositor.rs:377 — replaces LayerOutput.blit_to_led
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedRoute { None, Mirror, Direct }
// Built once at descriptor construction (:1851, :1958):
//   layer_type == Dmx → Direct
//   else blit_to_led  → Mirror
//   else              → None
```

(MVP-P3, D15: `Led` renamed to `Dmx`; the int is the only wire form on disk and `"Led"`
stays accepted on load, so no migration rung.)

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

**MVP-P2 — LED preset pack (two families, nine presets, split across two lanes).**

Peter (2026-09-03): "the next task is useful presets to give performance parameters, discrete
controls, edge triggered behaviors" + "Discrete controls for debugging or configure the lights
for photos in my studio… General lighting, but also performance presets." Hue/saturation params
for the whole pack (Vec4 color binding is deferred to its own pass — noted in the status header).

*Performance family (6 presets — lane A):*
- *LED Chase Sweep* (smooth) — one-comet-width comet sweeps horizontally across all strips;
  speed in beats/bar, tail length, direction, hue/sat.
- *LED Pulse* (smooth) — brightness breathes up all strips vertically; period in beats, hue/sat.
- *LED Step Chase* (stepped) — comet jumps K LEDs per note division (16th default), hard edges,
  constant between divisions; steps count, direction, hue/sat. Discrete control: the 8×120 grid
  reads hard steps better than sweeps.
- *LED Step Scan* (stepped) — a column lights one strip at a time, stepping per division.
- *LED Burst* (trigger) — pad hit fires a strobe burst (N flashes over M beats, decaying duty);
  retriggerable; flashes + rate params, hue/sat.
- *LED Cycle* (trigger) — each pad hit advances a variant (comet color/position) via
  `trigger_count`; BasicShapes.json is the composition precedent.

*Utility family (3 presets — lane B):*
- *LED Studio Light* — white/tinted fill with a real **brightness Float param (0–1)** through a
  multiply atom (LED Fill pins value at 1 by design — this preset exists because photo work
  needs 10–40%); temperature via hue at low saturation. Self-contained, OSC-able.
- *LED Strip ID* — distinct band per column (or a stepping white column): identifies which
  physical strip is which universe. Doubles as the re-patching tool while the venue-patch UI
  is deferred.
- *LED Pixel Walk* — a single white pixel steps through the whole grid in linear order: finds
  dead LEDs and reveals reversed/top-down wiring.

- *Entry state:* MVP-P1 shipped. Anchors: `ls crates/manifold-renderer/assets/generator-presets/`
  (LED Fill.json is the shape + description-format precedent), `rg -n "beat_ramp|trigger_count"
  crates/manifold-renderer/assets/generator-presets/` (beat/trigger wiring precedents),
  BasicShapes.json end-to-end (trigger cycling precedent).
- *Read-back:* restate D3/D9/D12 + the family spec above; read LED Fill.json + BasicShapes.json
  whole.
- *Deliverables:* the presets of your family as bundled JSON (`LED ` name prefix, UV-space,
  `presetMetadata` params/bindings with Float/Enum/Bool/Int converts ONLY — no Vec4), each
  pre-flighted through `graph-tool validate --kind generator` AND `graph-tool fusion`; the
  section 2.5 audit recorded per preset (exists / one wire away / genuinely new — genuinely new
  ESCALATES, never a quiet new primitive); a per-preset value test on the LED composite at
  8×120 vs CPU-computed expected output (harness precedent: the led_composite_pixel_tests in
  layer_compositor.rs — extend, don't rebuild); `docs/node_catalog.json` + `NODE_CATALOG.md`
  regen + the fused-WGSL golden regen (freshness gate — diff must contain ONLY your presets'
  blocks).
- *Gate (positive):* family behaviors verified by value: Chase position monotonic in beat
  phase; Strobe/Burst flips state at the duty point; Step Chase constant between divisions and
  jumps exactly at division boundaries; Pixel Walk index advances one cell per step in linear
  order; Strip ID columns pairwise-distinct. Default-preset contract from P1 still green (LED
  Fill untouched or grown under the same id).
- *Gate (negative):* zero new primitive files; `rg -n '"convert": \{\s*"type": "Vec4"'`
  returns zero hits in your presets; no fixed-pixel-count authoring (all graphs UV-space —
  a `rg` on literal 120/8 dims in node params is a smell to justify or remove).
- *Acceptance demo:* L2 — headless PNG strip of each preset at 8×120 for Peter; click-script
  (L4, Peter): pad-fire each on the real rig.
- *Performer gesture:* fire Step Chase from a pad; it advances one hard step per 16th at the
  track BPM and stops clean on clip end. Gate exercises the gesture via the existing
  phantom-clip path.
- *Forbidden moves:* new primitives without the 2.5 audit + escalation; Vec4 bindings
  (deferred, its own pass); fixed-pixel authoring; shipping without validate + fusion;
  touching the P1 gates or the send path.
- *Test scope:* renderer crate tests + graph-tool pre-flight; gpu-proofs gate (fusion golden
  touched — mandatory).

**MVP-P3 — DMX lane first-class (rename, browser category, card fixes; bead BUG-ng6a (DMX lane first-class campaign)).**
Three sessions, ordered so behavior lands before the rename and the rename before UI data
work. Peter (2026-09-04): "the LayerType should be updated to DMX … the easiest time we
will ever have to refactor base stuff like this"; "LED presets should be a unique category
in the effects browser"; "we must handle all UI and UX, commands, drivers, edge cases,
LFOs, triggers."

**MVP-P3a — behavior fixes: gen-carrying predicate + guard widening (vertical slice).**
- *Entry state:* MVP-P2 on main; `cargo nextest run -p manifold-core -p manifold-playback`
  green at entry.
- *Read-back:* D16, D19, D21; beads BUG-ev8u (generator picker no-ops on LED/DMX lanes) and
  BUG-p3rq (modulation reset gated to Generator lanes). Re-verify anchors:
  `rg -n 'change_generator_type|restore_generator_state' crates/manifold-core/src/layer.rs`,
  `rg -n 'reset_all_effectives' crates/manifold-playback` — counts must match this doc
  (3 guards in layer.rs, 1 in modulation.rs) or stop and re-list.
- *Deliverables:* one predicate on `Layer` — `hosts_generator()`, type membership per D16 —
  replacing the `!= LayerType::Generator` early returns in `change_generator_type`
  (layer.rs:884), `restore_generator_state` (:945), and the identity reconcile (:904); the
  `== LayerType::Generator` gate in `reset_all_effectives` (modulation.rs:204) reads the
  same predicate. The widening also covers `PasteGeneratorCommand`
  (crates/manifold-editing/src/commands/settings.rs:487-505), which reaches the model
  through `restore_generator_state`. **No production change in `timeline.rs`** — its LED
  arm calls `change_generator_type`, which the widening fixes automatically (adversarial
  review finding; the pre-amendment "seed gen_params in timeline.rs" deliverable was
  wrong-shaped). Tests: the two characterization tests below, plus a paste-onto-DMX-lane
  regression through PasteGeneratorCommand, plus an `add_layer` regression (Led layer +
  "LED Fill" → gen_params seeded).
- *Gate (positive):* (1) unit — build a `LayerType::Led` layer with the LED Fill default,
  run `ChangeGeneratorTypeCommand`, assert `generator_type` changed and undo restores —
  fails on pre-fix code, mandatory; (2) unit — envelope on a DMX-lane generator param
  returns to base after the driving note ends (the BUG-p3rq (modulation reset gated to Generator lanes) ratchet) —
  fails on pre-fix code, mandatory; (3) paste a generator onto an empty-aside-from-type
  Generator layer still assigns (the predicate-shape regression the adversarial review
  caught) — fails if the predicate is presence-based.
- *Gate (negative):* `rg -n '!= LayerType::Generator' crates/manifold-core/src/layer.rs`
  returns zero hits outside the predicate definition itself.
- *Demo:* L3 — ui-flow: select LED layer → click the gen card's Change button → pick LED
  Pulse → assert the card header reads "LED Pulse". Pre-fix behavior for comparison:
  header stays "LED Fill".
- *Performer gesture:* fire Step Chase from a pad on the rig; the swap means the whole
  preset pack is one click away per lane.
- *Forbidden moves:* touching the card/reconcile machinery (D19 — the model guard is the
  whole defect); widening the drag-move / scene-panel / MIDI-import guards (Peter calls,
  D16); any change to the send path.
- *Test scope:* `cargo nextest run -p manifold-core -p manifold-playback -p manifold-editing`;
  clippy same set. No GPU path touched.

**MVP-P3b — rename `LayerType::Led` → `LayerType::Dmx` (compiler-driven).**
- *Entry state:* P3a landed and merged.
- *Read-back:* D15, D18; DESIGN_DOC_STANDARD.md section 6 (seam briefs, compiler-driven migration).
  Re-derivation command:
  `rg -n 'LayerType::Led|is_led\(\)|"Led"' crates/ --no-heading -g '*.rs' | wc -l` — the
  audit counted ~60 non-test sites in 14 files; if the count differs materially, stop and
  re-list before touching anything. The P3a tests name `LayerType::Led` — they are
  expected rename call sites and belong in the seam inventory.
- *Deliverables:* rename the variant + `is_led()` → `is_dmx()` (keep `routes_to_led` —
  it names the LED composite incl. mirror routing); serde Deserialize accepts `4`,
  `"Dmx"`, `"Led"` (Serialize unchanged, int only); delete the dead `led_tap` path
  (BUG-zu92 (delete or confirm dead led_tap compositor path)) once no reference remains;
  widen `io/collect.rs:142` string-param gate to the D16 predicate
  (BUG-bbg5 (string-param collect gated Generator-only in io/collect)); doc prose sweep
  (the `Led = 4` pin in this doc's history is updated by this landing's status edit).
- *Gate (positive):* round-trip via `project_tool` — save a project containing a DMX
  layer, reload, assert type is Dmx AND a hand-built JSON with `"layerType": 4` and one
  with `"layerType": "Led"` both load as Dmx (the silent-fallback-to-Video failure mode
  is what this pins).
- *Gate (negative):* `rg -n 'LayerType::Led\b' crates/` returns zero hits except the serde
  alias arm; `rg -n '\bLed\b' crates/manifold-core/src/types.rs` likewise.
- *Demo:* L1 — no user-visible surface changes; the P3a L3 flow re-run as regression.
- *Forbidden moves:* renaming `LED *.json` preset ids/filenames (D18); a migration rung
  (D15); renaming `settings.led_*` persisted keys (they're settings, not the layer type —
  the `manifold-led` crate and its settings keep their names).
- *Test scope:* touched-crate nextest + clippy (core, io, editing, playback, renderer,
  app, ui); gpu-proofs NOT needed (no kernel/graph change — D9's render path is
  identifier-only).

**MVP-P3c — browser: LED category + generator chips + lane-scoped open.**
- *Entry state:* P3b landed. Ten `LED *.json` presets on disk with `category: "Pattern"`.
- *Read-back:* D17, D18. Re-verify anchors:
  `rg -n 'category: None' crates/manifold-core/src/preset_type_registry.rs`,
  `rg -n 'tag_project_category' crates/manifold-app/src/ui_root/dropdowns.rs` (must match
  the audit's two call sites).
- *Deliverables (the audit's six steps, in order):* (1) `preset_type_registry.rs:111`
  stops discarding generator JSON categories (`category: Some(leak(&preset.category))`);
  (2) the ten LED preset JSONs re-tag `category: "LED"` (line 8 of each), each
  pre-flighted through `graph-tool validate --kind generator`; (3) generator mode passes
  categories through `build_preset_picker_items` and builds `category_names` by the same
  derivation Effect mode uses (dropdowns.rs:364-370); (4) `category_color`
  (browser_popup.rs:996) gains an "LED" arm (the "Generators" chip-label skip at
  browser_popup.rs:566 is effect-browser-specific and stays — say so, or a lane will
  "fix" it); (5) scoped open: the GenTypeClicked arm calls the existing
  `set_category(Some("LED"))` (browser_popup.rs:323) **after `open()` in the same arm** —
  `set_category` no-ops while the popup session is None, and the session is created inside
  `open()` (:296), so calling it before/without open() is a silent dead end; the arm needs
  the requesting layer's type from the project snapshot — if UiRoot has no reach to it,
  escalate; do NOT add a request field; (6) no `BrowserPopupRequest` schema change.
- *Gate (positive):* (1) `graph-tool validate --kind generator` clean on all ten re-tagged
  presets; (2) unit — a Generator-mode picker request built for a DMX layer carries the
  LED category active and items filtered to `category == "LED"`; (3) the P1 default-preset
  contract still green (LED Fill id unchanged — D18).
- *Gate (negative):* `rg -n '"category": "Pattern"' crates/manifold-renderer/assets/generator-presets/`
  returns zero hits in `LED*.json`; `rg -n 'category: None' crates/manifold-core/src/preset_type_registry.rs`
  returns zero hits outside inventory-generator registration.
- *Demo:* L3 — ui-flow: open the generator browser from a DMX lane → assert the LED chip
  is active and only the ten LED presets are listed; switch the chip to "All" → the full
  generator list returns.
- *Forbidden moves:* a DMX-only browser or request-mode fork (D17); renaming preset ids
  (D18); hard-filtering (scoped open is a *starting* chip the performer can widen — the
  audit's step 6 deliberately reuses `set_category`).
- *Test scope:* `cargo nextest run -p manifold-core -p manifold-app -p manifold-ui`;
  clippy same set. No GPU paths.

*Deferred by decision, not by omission:* scene-panel scope (D20, BUG-p8ma (decide Scene Setup panel scope for DMX lanes)),
drag-move intent (BUG-6nar (layer drag-move generator predicate excludes LED/DMX lanes)),
MIDI import meaning (BUG-xlrx (MIDI import treats LED lane as video lane)), percussion
import (BUG-d0gb (percussion import scans Generator lanes only)), muted-lane GPU skip
(D21, BUG-n6dm (skip generator GPU work for muted/invisible LED lanes)) — each is a Peter
call or its own trigger, tracked as beads.

**MVP-P4 — LED composite preview blit at the top of the DMX lane inspector (2026-09-04,
Peter: "add a small preview blit to the LED layer inspector at the top … useful as a debug
tool and preview screen"; must meet the standard UI/UX quality bar).**
- *Entry state:* MVP-P3 on main; `dmxcard` ui-snap scene exists (P3c demo).
- *Read-back:* D22, D23, D24. Re-verify anchors: `rg -n 'try_read' crates/manifold-led/src/readback.rs`,
  `rg -n 'struct ContentState' crates/manifold-app/src` — stop and re-list if moved.
- *Deliverables:* (1) `try_read` returns `Arc<[u8]>` (readback.rs:97 already allocates
  this buffer per completed frame; the same allocation serves `pack_and_send` and the
  snapshot — no clone, no new per-frame alloc); the content thread publishes
  `led_preview: Option<LedPreview>` — an enum `Frame { pixels: Arc<[u8]>, version: u64 }`
  / `Black` / `None` — on ContentState (content_state.rs:66, built per tick), version
  bumped per completed readback; `Black`/`None` per D24's state table (blackout and
  disabled CLEAR the cached frame). (2) Inspector projection passes it to the UI when the
  active layer is a DMX lane. (3) UI: at the top of the DMX lane's generator card, a
  transposed 120×8 band rendered through the viewport bitmap path (viewport/render.rs:34,
  197,316 — dirty-flagged bitmap → GPU upload → layer-bitmap blit, viewport.rs:1474) —
  card chrome (radius, 1px border, dark bg), no label; upload on version change, not per
  frame. Only on `LayerType::Dmx` lanes. Orientation (transpose + both flips) pinned
  against `led_composite_pixel_tests`' known strip/LED mappings, written into the test.
  (4) L3 flow: `led-inspector-preview` on `dmxcard` — assert the preview node exists on
  the DMX card, assert a non-DMX layer's card has none (select FLOWERS, assert absence).
- *Gate (positive):* the L3 flow green; a ui-snap PNG of the DMX card with the band
  present (Peter looks — L2); an orientation unit test pinning transpose+flips against
  the known composite mappings; clippy clean.
- *Gate (negative):* `rg -n 'Arc<Mutex.*led_preview|Arc<RwLock.*led_preview' crates/` zero
  hits (D23); **no new content-thread per-frame allocation at all** (the Arc shares the
  existing readback allocation — state what, if anything, allocates and why).
- *Demo:* L3 flow + PNG for Peter's look.
- *Performer gesture:* kill the LED output mid-show — the band goes black in step with
  the strips; a chase reads as moving bands across the preview.
- *Forbidden moves:* a second GPU readback for the UI (D23); per-layer sampling (D22);
  shared-state handoff (D23); bespoke widget infrastructure beyond the blit (WIDGET_TREE
  discipline); placeholder/idle copy (D24); showing the preview on non-DMX cards.
- *Test scope:* `cargo nextest run -p manifold-app -p manifold-ui -p manifold-led
  -p manifold-core` (whichever the diff actually touches); clippy same; gpu-proofs not
  touched (no kernel change — the preview consumes the existing readback).

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
