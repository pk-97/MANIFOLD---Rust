<!-- index: The timeline layer-header card (manifold-ui/src/panels/layer_header.rs) renders a different control set per layer type. Today the type-branching is duplicated across four functions (layout, widget build, hit-test, data sync), so each new layer type means a parallel edit in all four and they can drift. This doc specifies the refactor: one declarative LayerControl descriptor list per type, walked by a single layout/build/hit-test engine. Existing types (video/generator/group) must reproduce byte-identical before audio is added. Audio's card (Mute, Solo, Gain, Send) is then just a new descriptor list. Covers the model, the engine, the per-type inventory grounded in current code, the gain-slider + send-dropdown widgets, the CoordinateMapper height constraint, and the build order. -->

# MANIFOLD — Layer Controls Design

Status: **SHIPPED** (verified in-tree 2026-07-05 baseline review: the `LayerControl`
descriptor engine, Gain/Send widgets, and the Analysis toggle + routing split are live
in `layer_header.rs` — this doc previously still said "design, not yet built"; its §5.3
"still to build" claims are also stale). Authored 2026-06-18 as prereq for the Audio
Layer card (`docs/AUDIO_LAYER_DESIGN.md`). Owns the timeline layer-header control surface;
the graph/node UI lives in `docs/UI_UX_SYSTEM_DESIGN.md`, which this does not touch.

---

## 1. The problem

The layer-header card ([layer_header.rs](../crates/manifold-ui/src/panels/layer_header.rs))
shows a different control set per layer type — video, generator, group. The
type-branching is spread across **four functions that each branch independently**:

| Function | Role | Branches on type |
|---|---|---|
| `compute_layer_row` | lays out the control rects into `LayerRowData` | group / generator / video |
| `build_layer_row` | creates the UITree widgets from those rects | `has_video_controls` / `has_generator_controls` / … |
| `handle_click` | hit-tests a clicked node id → `PanelAction` | per-rect id checks |
| state_sync (`manifold-app`) | fills `LayerInfo` from the model | per field |

Adding a layer type means a parallel edit in all four, kept in sync by hand.
That is the smell. Audio would be the fourth such branch; the cost compounds
with every future type. The fix is to make the control set **data**, not four
code paths.

## 2. The model

One descriptor enum names every control the card can show. A per-type function
returns the ordered list for that layer.

```rust
enum LayerControl {
    Chevron,        // collapse toggle (all types)
    Name,           // editable name (all)
    DragHandle,     // reorder grip (all)
    Mute,
    Solo,
    Led,            // LED-output toggle — NOT lock; only LED layers
    Blend,          // blend-mode dropdown
    Info,           // "N clips" summary line
    GenType,        // generator type label
    Folder,         // video source folder + path
    NewClip,        // "+ new clip"
    AddGenClip,     // generator "+ new clip"
    MidiNote,       // note + trigger-mode toggle (one row)
    MidiChanDevice, // channel + device dropdown (one row)
    Gain,           // NEW — dB slider (audio)
    Send,           // NEW — modulation-send dropdown (audio)
    Separator,
}

fn controls_for(layer: &LayerInfo) -> Vec<LayerControl>;
```

Each `LayerControl` carries its own intrinsic height/row behaviour, so the list
order plus a running `y` is enough to lay out the whole card. `LayerRowData`'s
hand-named rect fields (`mute`, `blend_mode`, `folder`, …) collapse into a
`Vec<(LayerControl, Rect)>` keyed by the descriptor.

## 3. The engine

Three passes, each walking the descriptor list once — replacing the three
type-branches with one loop:

- **Layout** — `controls_for(layer)` → fold a running `y`, emitting `(control, rect)`.
  This is the new `compute_layer_row`: no `if is_generator / is_video`, just the list.
- **Build** — for each `(control, rect)`, `match control` to the widget call.
  One match arm per control kind, shared by every layer type.
- **Hit-test** — the clicked node id maps back to its `(control, rect)`; the
  control kind plus layer id produces the `PanelAction`. No per-rect id ladder.

The descriptor → `PanelAction` mapping is the one place a control's behaviour
lives, so layout/build/hit-test can never drift for a control again.

## 4. Per-type inventory (grounded in current code)

Reproduced exactly from `compute_layer_row` today, so the refactor is a faithful
restatement, not a redesign:

| Type | Controls (in order) |
|---|---|
| **All** | Chevron, Name, DragHandle, then Mute, Solo, Led, Blend |
| **Collapsed (non-group)** | the above + Separator (early stop) |
| **Group** | + Info |
| **Generator** | GenType (before the M/S row), + Info, AddGenClip, MidiNote, MidiChanDevice |
| **Video** | + Info, Folder, NewClip, MidiNote, MidiChanDevice |
| **Audio (new)** | Chevron, Name, DragHandle, Mute, Analysis, Solo, Gain, Send |

Audio omits Led (LED output is unrelated), Blend (no compositing), Folder /
NewClip (files arrive by drag-drop), GenType, and all MIDI (no note triggering).
Decisions confirmed with Peter 2026-06-18: cut "+ new clip" and the third
(Led) button; Mute/Solo are the same shared buttons as other layers. The
**Analysis** toggle (output state, §5.3) was added 2026-06-19.

## 5. Audio's new controls

### 5.1 Gain

**Gain (`LayerControl::Gain`)** — a horizontal **dB slider**, extracted as a
reusable widget so master and clip gain can reuse it later. Drives
`Layer.audio_gain_db` via the existing `SetLayerAudioGainCommand`. Drag → live
value; release → one undo step. This widget is the real "shared friendly API"
the refactor yields; everything else is restatement.

### 5.2 Send

**Send (`LayerControl::Send`)** — a **dropdown** picking which modulation send
this layer feeds, reusing the *existing* dropdown mechanism (the same overlay
blend-mode and the device picker open via a `*Clicked(index)` action). **Shipped
2026-06-19.** Routes the layer to a send via `SetLayerAudioSendCommand`, which
adds the layer to that send's `source.layers` set ([audio_setup.rs](../crates/manifold-core/src/audio_setup.rs))
and detaches it from any other — a layer feeds **at most one** send; the send
keeps its capture channels + other layers (a default send becomes a capture+layer
mix). The send then reads the layer's realtime post-fader tap.

Send routing is **one mutation** (`SetLayerAudioSendCommand`); the layer
dropdown and the Audio Setup panel's source button both drive it, so they cannot
disagree. (Open: whether to keep the Audio Setup button once the layer dropdown
ships — see AUDIO_LAYER_DESIGN.)

### 5.3 Output state — the Analysis toggle

**Locked with Peter 2026-06-19.** Every audio lane has **three output states**,
driven by **two independent toggles** on the header — **Mute** (the existing
shared button) and a new **Analysis** toggle. Stem lanes from Detect and Group
([AUDIO_CLIP_DETECTION_DESIGN §8](AUDIO_CLIP_DETECTION_DESIGN.md)) default to
**Analysis**.

| State | Toggles | → master | → send | Meaning |
|---|---|---|---|---|
| **Live** | Mute off, Analysis off | ● audible | ● feeds | normal: heard and modulating |
| **Analysis-only** | Mute off, Analysis on | ✕ silent | ● feeds | silent, still listening (drives visuals) |
| **Muted** | Mute on | ✕ silent | ✕ none | fully off — mute wins over Analysis |

- **Why a third state, not just mute.** The shipped send tap is **post-fader**,
  and the shipped mute path zeroes the sub-track volume
  ([audio_layer_playback.rs:226](../crates/manifold-playback/src/audio_layer_playback.rs#L226)),
  so **mute already kills the send.** "Silent to master but hot to its send" is
  therefore a distinct routing, not a fader move — see AUDIO_LAYER_DESIGN §5 for
  the routing split. **Status:** Mute/Solo/Gain/Send shipped; the **Analysis
  toggle + its routing split are the part still to build.**
- **Visual.** Analysis-only reads as an **ear / scope glyph + a dimmed
  waveform** — output-off is implied by the dim, the glyph says "still
  analyzed." It must never look like a plain mute.
- **The two toggles are independent;** Mute dominates. (Mute on → Muted
  regardless of Analysis.)

> ✓ **Settled by the shipped code** (was flagged as a reversal). An early draft
> of AUDIO_LAYER_DESIGN §5 wanted mute to keep feeding the send. The shipped
> infra does the opposite — the post-fader tap zeroes on mute, so **mute is
> already the "fully off" state.** The 3-state model matches what shipped; no
> decision is owed. Analysis-only is purely the additive middle state.

Command surface: the Analysis toggle is a per-layer state mutation following the
existing `Set*` command pattern (sibling to mute/solo), serialized on the layer.

## 6. Height & the Y-layout constraint

Per-layer card height is **not** chosen in the panel — it comes from
`CoordinateMapper` ([coordinate_mapper.rs:140](../crates/manifold-ui/src/coordinate_mapper.rs#L140)),
the single source shared with the timeline tracks (the `single-source-y-layout`
/ `track-header-invariant` rules). It already branches on `layer_type` +
collapsed/group state. So:

- Keeping audio cards the **same height** as other layers needs no Y-layout change.
- A **shorter** audio card is a one-line branch *in CoordinateMapper* (a
  `COLLAPSED_AUDIO_TRACK_HEIGHT` style constant), which keeps the header and the
  track in lockstep automatically. It must never be set in the panel alone.

Default: same height for now; revisit a shorter audio card as a follow-up.

## 7. The equivalence gate

This rewrites a **live perform-mode panel**, so existing types must be proven
unchanged before audio is added:

1. The descriptor engine reproduces video / generator / group with the **same
   rects and the same UITree node structure** as the current code.
2. `layer_header.rs`'s existing panel tests stay green; add a test asserting the
   descriptor-driven layout equals the old `compute_layer_row` output rect-for-rect
   for each type (the same flatten-equivalence discipline used for node groups).
3. Visual parity check on a real project before audio lands.

Audio is **not** added until 1–3 pass. This is the gate, not a nicety.

## 8. Build order

1. Define `LayerControl` + `controls_for`; port `compute_layer_row` to fold the
   list — existing types only, rects identical, tests green. **(gate)**
2. Port `build_layer_row` and `handle_click` to walk the descriptor list — node
   structure identical, tests green. **(gate)**
3. Extract the **dB gain slider** widget (no audio wiring yet; unit-test value↔px).
4. Add the audio descriptor list + the Gain and Send controls; fill the new
   `LayerInfo` fields in state_sync; route the two new `PanelAction`s to their
   existing commands.
5. (Optional follow-up) shorter audio card via a CoordinateMapper height branch.
