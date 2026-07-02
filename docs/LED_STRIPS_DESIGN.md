# LED Strips — Play the Strips + Generalized Patch

**Status: APPROVED design, not built · 2026-07-03 · Fable (from the live-rig discussions)**
**Prerequisites: none for P1 (patch generalization works against today's path). P2 (strip
island) rides the island model from `docs/MULTI_DISPLAY_DESIGN.md` P1–P3.**

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
  per-port IC type / pixel count / universe mapping configured **on the unit**. MANIFOLD's
  only job is to emit the universes the patch declares; controller internals never leak
  into the data model.
- Channel math: 120 px × 3 ch = 360 ch → **one universe per strip** (`PerUniverse`
  addressing, already the default). 8 strips = 8 universes.

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
  missing at implementation, the §2.5 audit rule applies (expect none — these are gradients,
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

## 5. Phasing (Sonnet-executable)

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
