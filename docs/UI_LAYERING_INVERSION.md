# UI Layering Inversion — Phase 5 (sub-design)

Sub-design for **Phase 5** of `UI_ARCHITECTURE_OVERHAUL.md`. Phases 0–4 are
complete; this is the last. The goal: `manifold-ui` emits **UI-local events** and
consumes **UI-local view-data**, the **app** maps both to/from the engine, and the
crate **compiles with no dependency on `manifold-core`** (the engine data layer).
That is what lets the UI stand alone, be unit-tested in isolation, and be driven by
an external design tool.

## The problem (as-built)

`manifold-ui` depended only on `manifold-core`, but 30 of its 64 files reached into
it: `PanelAction` (≈200 variants) carried `ParamId`, `AudioSendId`, `LayerId`,
`Beats`, `MidiTriggerMode`, `AbletonMacroAddress`, …; the `TimelineEditingHost`
trait and the panel setters consumed `Layer`, `TimelineMarker`, `ParamSlot`,
`SelectionRegion`. The UI was welded to the engine's type surface.

## The decision

Boundary types split into **two kinds**, handled two ways. The split is the
ports-and-adapters principle applied honestly: **share value objects, adapt domain
entities.**

### 1. Shared primitive vocabulary → `manifold-foundation` (new zero-dep crate)

Types that are genuinely the *same concept* on both sides — an id is an id, a beat
is a beat. Duplicating these would be pure ceremony and, for `Beats`, a
value-parity hazard (its arithmetic is load-bearing in coordinate math). They live
in a new leaf crate both layers depend on:

- `units` — `Beats`, `Seconds`, `Bpm`, `beats_to_seconds`, `seconds_to_beats`
- `id` — `ClipId`, `LayerId`, `EffectId`, `EffectGroupId`, `MarkerId`, `NodeId`,
  `AudioSendId`
- `ParamId` (the `Cow<'static, str>` alias)

`manifold-core` **re-exports all of these at their current paths** (`manifold_core::Beats`,
`manifold_core::id::LayerId`, `manifold_core::effects::ParamId`, …) via shim
modules, so **no other crate changes** and project-file serialization is
byte-identical (the moved code is verbatim; the canonical Liveschool fixture is the
regression gate). `manifold-ui` depends on `manifold-foundation` instead of
`manifold-core`. Because these are the *same type* on both sides, the ≈40 events
that carry an id/beat need **no translation**.

### 2. Domain semantics → UI-local mirror/view-model, app translates

Types whose meaning belongs to the engine domain, or whose UI-facing shape
legitimately differs from the stored model. These get a UI-local definition in
`manifold-ui` and the **app is the sole translator**. No core surgery.

- **Mirrored enums/data** (`manifold-ui/src/types.rs`): `LayerType`,
  `MidiTriggerMode`, `TonemapCurve`, `DriverWaveform`, `MarkerColor`, `MacroCurve`,
  `AudioBand`, `AudioFeatureKind`, `AudioFeature`, `AudioDeviceRef`,
  `AbletonMacroAddress` (+`AbletonDeviceIdentity`), `ParamConvert`,
  `SerializedParamValue`, `PresetTypeId` (the UI keeps the `Cow` newtype; the
  registry-querying methods stay in core), constants `MACRO_COUNT`/`FLOOR_DB_OFF`,
  and `note_number_to_name`.
- **View-models** (`manifold-ui/src/view.rs`): `UiLayer` (the field subset the
  Y-layout / headers / inspector read), `UiParamSlot` (`value`,`base`,`exposed`),
  `UiMarker`. `SelectionRegion` already had a UI-local twin in
  `viewport/model.rs`; that becomes the single one (the core-typed uses in
  `ui_state`/`timeline_editing_host` move onto it).

`PresetTypeId` is **not** moved to foundation: its methods iterate core's
`inventory`-based effect/generator registries, so it is not a leaf type.

## The translation boundary (app)

`manifold-app` gains `ui_translate.rs`: free functions `to_core_*` / `from_core_*`
for every mirrored enum/struct, plus the `core → Ui*` view-model builders used when
pushing render data (`set_markers`, `sync_values`, `rebuild_mapper_layout`, the
`TimelineEditingHost::layers()` cache). This is the single reconciliation point —
the place where the UI vocabulary and the engine vocabulary meet, by design.

## Done when

- `manifold-ui/Cargo.toml` lists `manifold-foundation`, **not** `manifold-core`.
- `cargo build -p manifold-ui` and `cargo test -p manifold-ui --lib` pass.
- Full workspace build + `clippy -D warnings` green; Liveschool fixture loads.

## Deferred moves that this unblocks (ride after 5.1–5.3)

Recorded by earlier phases, now possible:
- Relocate the graph canvas out of `manifold-app` (4.2).
- Generalize `IntentRegistry` off `PanelAction` (4.5).
- One shared `process_events`/`InputHandler` for the editor window (4.6).
