# Param Step Actions — triggers step, randomize, and sequence any param

**Status:** PROPOSED design, awaiting Peter's read · 2026-07-07 · Fable
**Prerequisites:** LIVE_AUDIO_TRIGGERS §9 unification (SHIPPED 2026-07-07 @ `14e0a90a`) — this design extends the unified `ParameterAudioMod`, which must exist as landed.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight: the §8/§9 trigger work built a general event system (edge
detection, fire modes, pulse plumbing, drawer UI) whose only response so far is
"bump a trigger count". This design adds a second response family: **a trigger
event steps a param** — next value, previous, random, ±N with wrap — turning any
discrete slider into an audio-clocked sequencer and any continuous slider into
clocked sample-and-hold, on the perform surface, with zero graph editing. Peter,
2026-07-07: *"It would let you pseudo make any discrete slider act as a trigger"*
and *"This will multiply the types of visuals and ways they react and behave very
easily."* On stage: the kick steps BasicShapes' variant, every 4th snare
randomizes the palette, one transient advances pattern + reseeds + bumps hue
together, phase-locked — none of which is reachable today without editing graphs.

Two of Peter's calls decide the shape (both 2026-07-07, this session):
- *"I think step replacing the base value makes sense"* — a step acts like the
  user's hand moved the slider (shadowing the base), and everything that stacks
  on a hand-set base (drivers, continuous audio mods, envelopes) stacks on the
  stepped value identically (D4).
- *"Sounds like we will want to keep the discrete trigger as events so the clip
  trigger single button can stay as useful infra around that"* — the
  `trigger_count` event stream and the trigger-gate cards STAY. Step actions are
  a sibling response to the same events, not a replacement for event-consuming
  graphs (D10; forbidden move F6).

Companions: `LIVE_AUDIO_TRIGGERS_DESIGN.md` §8–§9 (the event system this rides
on — read §9 before any phase); `AUDIO_MODULATION_DESIGN.md` §10 (drawer
mechanics); `AUTOMATION_LANES_DESIGN.md` §4 (the base-writer contract steps
shadow).

## 1. Audit — what exists (verified 2026-07-07, against `14e0a90a`)

| Piece | Where | State |
|---|---|---|
| Unified per-param mod config (`ParameterAudioMod`: enabled, source, shape, `trigger_edge`, `fire_count`, `trigger_mode`) | `crates/manifold-core/src/audio_mod.rs:344-381` | exists — extend with one field (D1) |
| Pure armed/re-arm edge detector (`TransientEdge`, `REARM_RATIO`) | `crates/manifold-core/src/audio_trigger.rs:59-91` | exists — reuse unchanged |
| Fire-mode gating (`TriggerFireMode` Clip/Transient/Both + `wants_*`) | `crates/manifold-core/src/audio_trigger.rs:99-121` | exists — reuse for step sources (D3) |
| Evaluator with three per-target arms (gate-pulse / fire-count / continuous) | `crates/manifold-playback/src/modulation.rs:390-462` | exists — add a fourth arm (D2) |
| Pipeline order: automation → reset base→value → drivers → audio mods → envelopes | `modulation.rs:284-319` (order), `automation.rs:112-115` (base writes pre-reset) | exists — step apply slots after reset (D4) |
| `Param { base, value }` split; modulation writes `value`, never `base` | `crates/manifold-core/src/params.rs:36-60` | exists — step state shadows `base`, serialized project untouched |
| Deterministic per-cycle random (integer hash, Unity HashToFloat port) | `crates/manifold-core/src/effects.rs:2790-2801` | exists — the house random; reuse for D7 |
| Non-repeat invariant for cycling (`ClipTriggerCycle`) | `crates/manifold-renderer/src/generators/clip_trigger.rs` | exists renderer-side — the *invariant* moves into the step evaluator (D7); the type stays for graph consumers |
| Discrete-param vocabulary (`whole_numbers`, `value_labels`) | `crates/manifold-core/src/effect_graph_def.rs:456-463` | exists — defines "discrete slider" |
| Offline export feeds real analysis into the same tick ("param modulation, param triggers, and live clip triggers — deterministic audio reactivity") | `crates/manifold-app/src/content_export.rs:439-451` | exists — step actions inherit export support with no new work |
| Clip-edge observation (renderer-side): `acquire_clip` + `clip_count`/`audio_count` | `crates/manifold-renderer/src/generator_renderer.rs:44-92,350-360` | exists — stays for gate cards; steps get an ENGINE-side edge (D5) |
| Trigger division precedent (`tc_divided` in BasicShapes' Rotation Sequencing group) | `crates/manifold-renderer/assets/generator-presets/BasicShapes.json` | exists in-graph — generalized as `every` (D6) |
| Drawer UI: standard audio-mod drawer + trailing Mode row; command family (`SetAudioModTriggerModeCommand`) | LIVE_AUDIO_TRIGGERS §9.2 U-P2; `param_slider_shared.rs` (`build_toggle_trigger_row`) | exists — Action rows extend the same drawer (D8/P3) |

Classification: the event sources, edge detection, config home, evaluator walk,
export determinism, drawer chassis, and editing-command family all **exist**.
Genuinely new: one enum field + its evaluation arm, an engine-side per-layer
clip-edge event, the step-state lifecycle, and three drawer rows. This design is
mostly wiring; the audit is why.

## 2. Decisions

- **D1 — One config type, one new field.** `ParameterAudioMod` gains
  `action: TriggerAction` (serde default `Continuous`, skip-when-default — old
  projects and ordinary mods stay byte-identical). NO new config struct, NO new
  per-instance field. Rejected: a parallel `ParamStepMod` type, because §9 just
  paid a same-night two-bug tax to delete exactly that shape ("the walker
  forgets the second config type" bug class); the §9 U1 lesson is a hard
  precedent, not a preference.

- **D2 — The action enum.**
  ```rust
  // crates/manifold-core/src/audio_mod.rs
  #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "camelCase", tag = "kind")]
  pub enum TriggerAction {
      /// Today's behavior: the shaped signal overwrites the value continuously.
      #[default]
      Continuous,
      /// Each fire moves the stepped value by `amount` (signed, param units).
      Step { amount: f32, wrap: WrapMode },
      /// Each fire jumps to a deterministic pseudo-random value in range,
      /// never repeating the current one (D7).
      Random,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "camelCase")]
  pub enum WrapMode {
      /// min..=max is a cycle: stepping past max lands at min (and vice versa).
      #[default]
      Wrap,
      /// Ping-pong: direction reverses at min/max.
      Bounce,
      /// Saturate at the ends.
      Clamp,
  }
  ```
  Plus one divisor field on the mod itself (not inside the enum, so the drawer
  row layout is uniform): `every: u32` (serde default 1, skip-when-1) — fire the
  action on every Nth trigger event (D6). Evaluation for `whole_numbers` /
  `value_labels` params rounds the stepped result to integers; `amount` defaults
  to 1.0 there and to `(max-min)/8` for continuous params (UI seeding only —
  the stored value is whatever the user set).

- **D3 — Event sources = the trigger events that already exist, gated by
  `trigger_mode`.** A step/random mod fires from (a) its own audio edge — the
  D5b chassis, `trigger_edge.advance(out_norm, 0.5)` over the mod's shaped
  source, identical tuning feel to trigger gates — and (b) the owning layer's
  clip edge (D5). `trigger_mode: Option<TriggerFireMode>` gates which, exactly
  as it does for gate cards; `None` on a step mod means **Transient** (unlike
  gates' arm-time Both: a gate must not silently kill clip launches, but a step
  mod with no audio intent is meaningless — the user armed an audio drawer).
  Master-chain instances have no layer: clip contribution is 0, audio fires
  work (same rule as §8 D5). Rejected: a new source enum on the action —
  duplicates `TriggerFireMode` and forks the vocabulary the drawer just
  unified. Beat-clock and driver-edge sources are DEFERRED, not designed here
  (see Deferred — the ambiguity in Peter's "extend to the drivers" is flagged
  there and needs his call before anyone builds it).

- **D4 — Step replaces the base value (Peter, verbatim above).** Runtime state
  on the mod: `step_value: Option<f32>` + `step_dir: f32` (Bounce direction),
  both `#[serde(skip)]` like `smoothed`/`trigger_edge`. Lifecycle: first fire
  seeds from `p.base` then applies the action; each subsequent fire advances
  it; a disabled/deleted mod or project reload drops it (`None` → the param
  falls back to the committed base — kill the trigger and the slider returns
  to where you left it, deliberately, as live behavior). Apply site: a new
  **Phase 1.5** in `evaluate_modulation` (`modulation.rs:284-319`), immediately
  after `reset_all_effectives`: for every armed step/random mod with
  `step_value = Some(v)`, write `p.value = v`. Drivers, continuous audio mods,
  and envelopes then stack on top exactly as they stack on a hand-moved
  slider — no new precedence semantics invented.
  **Consequences, stated honestly:** (1) an automation lane on the same param
  writes `base` before the reset (`automation.rs:112-115`), so while a step mod
  is armed and has fired, the lane's value is shadowed — the step wins until
  it's disarmed. Same param under both is a performer conflict the design
  resolves by "last armed hand wins", not by merging. (2) A `ParameterDriver`
  on the same param overwrites `value` after Phase 1.5 from its own
  `base_value` field (`effects.rs:2618-2630`), so a driver beats a step — again
  identical to driver-vs-slider today. The drawer does not need to police
  these; the fallback is always the committed base. (3) Reload resets the
  stepped position — matches how `trigger_count`-driven cycles reset across
  sessions today.

- **D5 — The clip edge for steps is an ENGINE event, not renderer feedback.**
  Steps mutate playback-side param state, so the clip edge must be observed in
  playback. `sync_clips_to_time` is the sole authority for playback state
  (CLAUDE.md invariant); the engine adds a per-layer `last_active_clip_id`
  (AHashMap scratch, no per-frame alloc) updated in the tick and emits "layer N
  clip edge" flags consumed by the same modulation pass — mirroring what
  `acquire_clip` derives downstream (`generator_renderer.rs:350-360`) at the
  authority level. Rejected: a renderer→playback pulse backchannel, because it
  reverses the one-way dataflow for an event the engine already knows first.
  **Consequences, stated honestly:** the engine edge and `acquire_clip`'s edge
  can diverge by the renderer's clip-ready gating (a video clip that isn't
  decoded yet delays `acquire_clip` but not the engine's edge). For a step —
  a param move — firing at the musical moment is more correct than firing at
  texture-ready; accepted and documented here so nobody "fixes" it into sync.

- **D6 — `every: u32` divisor, default 1.** "Every 4th kick advances the
  pattern" is a musical necessity the graphs already prove (`tc_divided`,
  BasicShapes). Runtime fire-counter on the mod (serde-skip) increments per
  gated event; the action executes when `counter % every == 0`. The divisor
  counts events the mode admits (post-`trigger_mode`, pre-action).

- **D7 — Random is a deterministic hash of the fire ordinal, and never repeats
  the current value.** Reuse the HashToFloat integer hash
  (`effects.rs:2790-2801`) keyed by the mod's monotonic fire ordinal — no RNG
  state, no `rand` dependency, and offline export (which replays the same
  fires) reproduces the identical sequence run-over-run for free. Discrete
  non-repeat: with N reachable values, map `hash % (N-1)` and shift past the
  current index — the `ClipTriggerCycle` invariant (adjacent fires never emit
  the same value), enforced at the one place steps are computed. Continuous
  Random: full-range hash value (repeat probability ~0; no exclusion).
  Rejected: `thread_rng`/time-seeded randomness — breaks export reproducibility
  and the resume contract; this is the named temptation, see F4.

- **D8 — UI home: the same audio drawer, one Action row group.** On any
  non-toggle, non-trigger param card, the standard audio-mod drawer
  (§9 U2) gains: Segmented **Action** (Cont / Step / Rand); when Step —
  stepper **Amount** (signed, snapped for whole-number params) + Segmented
  **Wrap** (Wrap/Bounce/Clamp); when Step or Rand — stepper **Every** (1..16)
  + the Mode row (Clip/Audio/Both) that gate cards already render. Collapsed
  card row shows the action as a badge (the §8 "silent mode trap" rule).
  Commands: one new `EditingService` command per field, shaped like
  `SetAudioModTriggerModeCommand` (the smallest member of the existing
  audio-mod command family, §9.2 U-P2) — whole-old/new capture, one undo step.
  Trigger-gate cards (`is_trigger_gate`) and fire-buttons (`is_trigger`) do
  NOT get an Action row: their fire semantics are the count, by design.

- **D9 — Multi-param = one mod per param, no grouping infra.** "Choose
  multiple params" (Peter) is: add a step mod on each param, same send/band/
  sensitivity. Mods with identical source+shape settings see the same features
  in the same tick and fire together — phase lock falls out of the
  architecture. Rejected: a trigger-group entity binding N params to one
  config — new addressing surface, new walker arm, and the §9 lesson again;
  revisit only if L4 shows drift between same-source mods (it can't, short of
  differing sensitivity, which is user intent).

- **D10 — The event infrastructure stays; cycling graphs migrate per-preset,
  later, as their own wave.** Peter, verbatim: *"keep the discrete trigger as
  events so the clip trigger single button can stay as useful infra around
  that."* `trigger_count`, `generator_input.trigger_count`, gate cards, and
  event-consuming primitives (`trigger_gate`, spawn/inject groups,
  `scalar_array_accumulator`) are untouched. The class-1 cycling graphs
  (Plasma, BasicShapes variants, ConcentricTunnel, Wireframe, StrangeAttractor,
  MriVolume — `clip_trigger_cycle` call sites) become *candidates* to re-author
  as step actions on an exposed param; P4 proves the recipe on Plasma only, and
  the tranche is Deferred with its own trigger. Class-3 (FluidSim spawn/inject,
  reset_trigger) never migrates — a param step cannot express "inject an
  impulse this frame".

## 3. Data flow (committed)

```
tick:
  evaluate_all_automation            — lanes write base            (unchanged)
  reset_all_effectives               — base → value                (unchanged)
  apply_step_values      [NEW 1.5]   — armed step mods: value = step_value
  evaluate_all_drivers               — driver writes value         (unchanged)
  evaluate_all_audio_mods            — per mod, by action:
        Continuous       → overwrite value                         (unchanged)
        gate / is_trigger → pulse / fire_count                     (unchanged)
        Step / Random    → edge-detect (audio) + layer clip edge
                           (engine, D5), % every (D6) →
                           advance step_value (D4), snap+wrap (D2)
  evaluate_all_envelopes             — additive                    (unchanged)
```

The step arm lives in `evaluate_instance_audio_mods` as a fourth branch beside
the three at `modulation.rs:427-457`; `apply_step_values` is a small walk over
the same instance set (the `evaluate_all_audio_mods` walk shape, which
`automation.rs:11` already documents copying). Clip-edge flags travel as a
per-layer bitset/slice computed in the engine tick before modulation — no new
channel, no new thread, no shared state.

## 4. Phasing

### P1 — Core action model + audio-fired steps (one session)
- **Entry:** `14e0a90a` or later on main; re-verify the §1 anchors for
  `audio_mod.rs:344-381` and `modulation.rs:390-462`.
- **Read-back:** this doc §1–§3; LIVE_AUDIO_TRIGGERS §9.1 (U1–U6); restate D1,
  D4, D7 and forbidden moves F1–F4 before coding.
- **Deliverables:** `TriggerAction`/`WrapMode` + `action`/`every` fields
  (`audio_mod.rs`); runtime `step_value`/`step_dir`/divisor counter
  (serde-skip); the step arm in `evaluate_instance_audio_mods` (audio edge
  only — clip edge is P2); `apply_step_values` wired into `evaluate_modulation`
  Phase 1.5; hash-ordinal random with discrete non-repeat; snapping for
  `whole_numbers`/`value_labels`; `clear`-on-stop joins the BUG-051 path
  (`engine.stop()` already clears trigger edges — step state clears there too).
- **Gate (positive):** new `modulation::tests::step_*` covering: step advances
  base-shadow on fire only; wrap/bounce/clamp at both rails; every=N admits
  every Nth; random never repeats current discrete value across 200 fires;
  identical fire sequence ⇒ identical value sequence twice in a row
  (determinism, the export claim at L1); disarm falls back to committed base;
  serde round-trip with `action` set and absent (old-project bytes identical —
  assert on a fixture string). Focused: `cargo test -p manifold-core --lib`,
  `-p manifold-playback --lib`, clippy workspace.
- **Gate (negative):** `rg -n "thread_rng|SmallRng|rand::" crates/manifold-core/src crates/manifold-playback/src`
  → zero hits; `rg -n "struct ParamStepMod|step_mods" crates` → zero hits.
- **Demo:** none — L1 (no UI surface yet; the vertical slice lands in P3).
- **Forbidden moves:** F1–F4 (§6).

### P2 — Engine clip edge + mode gating (one session)
- **Entry:** P1 merged; re-verify `engine.rs` tick structure and
  `generator_renderer.rs:350-360` (the semantics being mirrored).
- **Read-back:** D3, D5 (including the divergence consequence — do NOT sync to
  renderer readiness); CORE_ENGINE_MAP.md §on sync_clips_to_time.
- **Deliverables:** per-layer active-clip-identity tracking in the engine tick
  (pre-allocated, AHashMap or Vec keyed by layer index — no per-frame alloc);
  clip-edge flags into the modulation pass; `trigger_mode` gating on step mods
  (`None`⇒Transient per D3); phantom/MIDI-launched clips produce edges (they
  are engine clip starts).
- **Gate (positive):** tests: timeline clip start fires a Clip-mode step;
  Transient-mode step ignores clip edges; Both sums; a clip *ending* (no new
  clip) fires nothing; live-slot launch fires. Focused playback suite + clippy.
  **Content-thread work gate:** `MANIFOLD_RENDER_TRACE=1` run on the canonical
  fixture (53 layers) — no frame >20ms attributable to the new tracking
  (measured, not argued).
- **Gate (negative):** `rg -n "trigger_pulse|TriggerPulse" crates/manifold-renderer/src/generator_renderer.rs`
  unchanged vs main (proves no renderer backchannel was added).
- **Demo:** none — L1 (surface still P3).
- **Forbidden moves:** F5, F1.

### P3 — Drawer UI + vertical slice (one session)
- **Entry:** P1+P2 merged. Re-verify `build_toggle_trigger_row` /
  `build_audio_mod_drawer` shapes (§9.2 U-P2 moved them recently).
- **Read-back:** D8; AUDIO_MODULATION_DESIGN §10.2; the §9.2 U-P2 account of
  which drawer pieces are shared.
- **Deliverables:** Action/Amount/Wrap/Every rows per D8; collapsed-row action
  badge; `SetAudioModActionCommand` (+ siblings if field-per-command fits the
  family better — executor's call inside the committed family shape);
  PanelAction + dispatch + state_sync; UI seeding of `amount` defaults (D2).
- **Gate:** ui + app focused tests; workspace clippy; **round-trip gate** —
  configure a step mod, save, reload, fire, verify stepping resumes from
  committed base (BUG-036 rule: modulate *after* reload); headless PNG of the
  drawer open with Action=Step on a whole-numbers param (Plasma `pattern`) and
  on a continuous param (Bloom amount).
- **Acceptance demo (L3):** a `scripts/ui-flows/` flow that opens the drawer,
  sets Action=Step, and asserts the badge — plus the P1 determinism test rerun.
  **Performer gesture:** "point the Kick send at BasicShapes' `variant`, set
  Step/Wrap, play a 4-bar loop, watch the shape advance per kick and wrap" —
  exercised live by Peter at L4 (owed, logged in VERIFICATION_DEBT at landing).
- **Forbidden moves:** F2, F6.

### P4 — Exemplar preset re-author: Plasma (one session)
- **Entry:** P1–P3 merged. §2.5 audit of Plasma's graph (open the JSON, follow
  every wire from `pattern_cycle`).
- **Deliverables:** Plasma's `pattern_cycle` node (`clip_trigger_cycle`,
  Plasma.json node id 3) deleted; `pattern` exposed as a whole-numbers card
  (0..7, value labels if the variants have names); shipped preset seeds no step
  mod (user adds it — presets don't ship armed audio config); load-migration
  for existing projects holding the old card set (the param_storage_v14 shape
  is the template).
- **Gate:** `check-presets` green; gpu-proofs run for Plasma's module
  (`--features gpu-proofs`, its `gpu_tests`); headless render PNG at pattern
  0/3/7 read by the orchestrator (grep-silence rule: render and look);
  round-trip on a project saved with the OLD Plasma cards.
- **Acceptance demo (L2):** three PNGs, visibly different patterns, named in
  the landing report. The remaining five class-1 presets: Deferred (below).
- **Forbidden moves:** F6, F7.

**Phasing-completeness check:** design-body commitments → P1 (action model,
step/random/wrap/every/determinism, D4 lifecycle), P2 (clip-edge source, D3
gating), P3 (drawer, badge, commands, multi-param via per-param mods — D9 needs
no build), P4 (one migration exemplar). Deferred carries: beat-clock source,
driver-edge source, the remaining cycling-preset tranche, trigger-group
entity. No committed affordance is unowned.

## 5. Decided — do not reopen

1. Fires are immediate — no launch-quantize on step actions. Carried from §8
   D3 (Peter re-confirmed 2026-07-07: the quantize suggestion "is wrong").
2. Step replaces the base value; drivers/mods/envelopes stack on top; no new
   precedence system (D4).
3. One config type — `TriggerAction` on `ParameterAudioMod`; never a parallel
   step-config type (D1, §9's paid-for lesson).
4. Random = deterministic fire-ordinal hash, never repeats current discrete
   value; no RNG state anywhere (D7).
5. Step state is runtime-only; reload falls back to committed base (D4).
6. The `trigger_count` event stream and gate cards stay; event-consuming
   graphs (class 3) never migrate to steps (D10, Peter verbatim).
7. Clip edges for steps come from the engine, never from renderer feedback;
   the readiness divergence is accepted (D5).
8. `every` divisor lives on the mod, counts mode-admitted events (D6).

## 6. Forbidden moves (named for this design)

- **F1 — A parallel config/walker.** You will want a `ParamStepMod` struct or
  a separate `evaluate_all_param_steps` walk. No — §9 deleted that shape after
  it produced two same-night bugs; one field, one arm, the existing walk.
- **F2 — Writing `p.base` or issuing `EditingService` commands per fire.** A
  kick at 128 BPM would flood the 200-cap undo stack in 100 seconds and dirty
  the serialized project. Steps live in the runtime shadow (D4).
- **F3 — Graph-side stepping.** An "audio step" node inside the graph violates
  audio-stays-on-perform-surface (LIVE_AUDIO_TRIGGERS R3). The graph consumes
  values; the perform surface steps them.
- **F4 — Nondeterministic random.** `thread_rng`, time seeds, or per-run state
  break export reproducibility (D7) and the workflow-resume contract. The hash
  is committed; transcribe it.
- **F5 — Renderer→playback event backchannel.** The engine already knows clip
  starts first (D5). Do not plumb `acquire_clip` back into modulation.
- **F6 — "Simplifying" class-3 graphs while touching class-1.** Spawn/inject/
  reset trigger inputs are events by Peter's explicit call; migrating a cycling
  preset never licenses touching its neighbors' event wiring.
- **F7 — Preset migration without load-migration.** Deleting a card param that
  existing projects reference silently drops user state on load — the forbidden
  move of load paths (standard §5). param_storage_v14 is the template.

## 7. Deferred (with revival triggers)

- **Beat-clock step source** (fire every N beats/bar without audio — the
  `BeatDivision` vocabulary, same cycle math as `DriverWaveform::Random`).
  Revive when Peter asks for tempo-locked stepping without an audio send; it
  slots in as one more `TriggerFireMode`-adjacent source and MUST go through a
  design addendum here, not ad-hoc. **Flag for Peter's read:** his phrase
  "this would also extend to the drivers" (2026-07-07) may have meant exactly
  this — his call decides whether it joins v1 (it would land as its own phase;
  P1–P3 don't block on it).
- **Driver-edge source** (a square driver's rising edge fires a step) — same
  flag as above; cheaper to add after the beat-clock call is made.
- **Cycling-preset tranche** (BasicShapes variant, ConcentricTunnel, Wireframe,
  StrangeAttractor, MriVolume). Revive after P4's Plasma recipe survives
  Peter's L4; each is a one-session preset re-author following P4's brief shape.
- **Trigger-group entity** (one config firing N params). Revive only if L4
  shows same-source mods drifting — see D9 for why they can't.
- **MIDI-note step source.** Revive when the perform-surface MIDI mapping work
  (PERFORM_SURFACE design) lands its binding vocabulary; steps then bind like
  any other MIDI-fired action.
