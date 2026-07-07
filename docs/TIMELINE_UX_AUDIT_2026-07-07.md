<!-- index: 2026-07-07 headless UX audit of timeline / layers / layer-controls / interaction (final Fable window). Stage 1: the "dead LANES button" — click path proven alive at every headless layer; root cause = AUTOMATION_LANES §7 exposure never shipped (no way to create a first lane). Stage 2: PNG audit findings triaged into fixed-now / ranked spec items / Peter feel-pass. The ranked spec section is the work list for the next timeline-UX wave. -->

# Timeline UX Audit — 2026-07-07 (headless pass)

**Status: AUDIT COMPLETE; small fixes landed on `fix/timeline-ux-pass`; spec items ranked below, unbuilt.**
Method: every finding here is anchored to a headless render or a driven interaction
(`ui-snap` scenes + the `--script` driver, which as of this branch dispatches through the
real `ui_bridge` — see §4). Nothing in this doc is derived from reading code alone.

## 1. The "dead LANES button" — what it actually was

The reported live defect: clicking LANES in the transport bar does nothing (Peter,
2026-07-05, recorded on AUTOMATION_LANES_DESIGN + VD-001).

**Finding: the click path is not dead at any layer headless can reach.** Driven with a
real synthesized pointer through `UIRoot::pointer_event` → `process_events` → intent
resolution → the real `ui_bridge::dispatch` → `selection.automation_mode_visible` flip →
structural rebuild → lane strips appear/disappear, PNG-verified both directions
(`scripts/ui-flows/toggle-lanes.json` on the `automation` scene, exit 0, asserts green).

**Root cause of the symptom: the feature is unreachable, not broken.** Lane strips render
only for params that already have lanes (`ui_translate::layer_automation_lanes_to_ui`),
and the only way to create a first lane is Automation-Arm recording during playback —
AUTOMATION_LANES_DESIGN §7's param-chooser + "+" affordance (the designed birth path for
lanes: "wiggle the knob, then draw") was never built; the strip label is explicitly a
"read-only stand-in for Live's param-chooser dropdown" (`automation_lane_draw.rs`). In
every project without recorded automation, LANES toggles a zero-row layout change: the
button lights, nothing else changes. That reads as dead, and it is the #1 ranked spec
item below.

**Owed to Peter (L4):** confirm in the running app that (a) LANES lights when clicked,
(b) ARM + touching a param during playback records a lane and the strip appears. If (a)
fails live, the remaining suspect layer is the winit→UIRoot event seam only — everything
below it is proven.

## 2. Ranked spec items (design gaps — build in this order)

1. **Automation exposure (AUTOMATION_LANES §7, unshipped half)** — the param-chooser
   lane + "+" button on the expanded layer, and touch-to-select feeding the chooser.
   Without it the entire P1–P4 automation system is reachable only via live recording.
   Design already decided (§7, "decided: copy Ableton's model — Peter 2026-07-02");
   this is implementation, not design. Also add the first-point-draw path: choosing a
   param shows an empty strip; clicking the strip creates the lane via
   `AddAutomationPointCommand`'s existing `created_lane` semantics.
2. **Layer-header height contract reconciliation (TIMELINE_UI_REDESIGN §B/§D)** — the
   doc's decided model is TWO heights (compact ≈58px identity+mix / expanded ≈200px
   + routing form); the shipped app renders the routing form at every non-collapsed
   height (states scene: NORMAL/MUTED/SOLO all show FOLDER/MIDI/CHANNEL/DEVICE at
   ~140px). Either the two-tier contract is stale (then amend §B) or the shipped
   behavior is nonconforming (then wire the `Tall` stop `coordinate_mapper.rs` already
   defines). Interacts with item 1: §7 places the automation chooser in the *expanded*
   layer's advanced controls, so the expanded tier must exist as designed before the
   chooser has a home.
3. **Mute/solo state legibility (MOTION P3, already planned — evidence added)** —
   headless: a muted layer's LANE is pixel-identical to a live one (only the M chip
   differs); solo-active is BLUE (`SOLO_COLOR`, color.rs:382) which collides with the
   reserved selection blue AND disagrees with the other solo token
   (`SOLO_BTN_ACTIVE = AMBER_ACTIVE`, color.rs:558) used elsewhere. P3's D4 mute-dim +
   SOLO_COLOR deletion covers this; rank it next in the motion sequence rather than
   cherry-picking.
4. **Audio-layer card dead space (LAYER_CONTROLS §6 deferral)** — the audio card's
   controls occupy ~60px of a full-height header; the rest is empty identity color
   (audiosends scene). §6 already scopes the fix: a shorter audio height branch in
   `CoordinateMapper`, never in the panel.
5. **ARM idle/active two-reds (transport automation cluster)** — idle ARM is
   `RECORD_RED`, armed is `RECORD_ACTIVE`: two reds distinguishable only by shade,
   for a mode that changes what touching a param DOES (override vs record). Mirrors
   the REC pair deliberately, but REC's states are "not recording / recording" while
   ARM's are "touch = override / touch = writes into the arrangement" — a wrong read
   on stage writes automation into the show. Consider armed = distinct treatment
   (e.g. the AUTOMATION_LINE_COLOR family). Needs Peter's call; low build cost.
6. **Harness gaps found while auditing (tooling)** — all but (iii) FIXED this branch:
   (i) `--scroll` applied after the base render (every prior "scrolled" base PNG was
   unscrolled); the naive reorder then exposed that the header column bakes its Y
   offsets at BUILD time while lanes read scroll at draw — rendering without a
   re-sync draws scrolled lanes under unscrolled headers, a desync the live app
   can't produce (it rebuilds on scroll-dirty). Fixed: seed then re-sync; verified
   headers+lanes lockstep at 400px. (ii) interact-miss detection grepped for a
   "MISS: " prefix no verb emitted, so misses exited 0 with an after-PNG of an
   interaction that never happened; fixed structurally — verbs return
   `Result`, `InteractOutcome.missed` drives the loud-fail, `select:NOPE` now
   exits 1 with the dump. (iii) `sync_build` stomps the scene zoom every rebuild,
   so zoom interactions are headless-unobservable — acceptable until MOTION P6
   continuous zoom needs headless evidence; note it in the P6 brief.
7. **Empty-project state is a black void** (`empty` scene, new): no affordance
   toward creating the first layer — the context-menu path is undiscoverable.
   Zero priority for Peter's own rig; becomes real at commercialization
   (COMMERCIALIZATION_DESIGN's first-run experience should absorb it — an
   APP_SHELL R7-style registration, not a new surface).

## 3. Feel-pass list for Peter (logged, not decided)

1. **Landing-line flash (re-hooked this branch)** — drag a clip and release: a 240ms
   blue vertical flash at the landed beat, spanning the moved layers. Keep / kill /
   retime is the D15 gate (UI_CRAFT_AND_MOTION_PLAN); it was dormant since P1.4
   deleted its old trigger. One-line repro: move any clip.
2. **LANES / BACK / ARM live confirmation (VD-001 L4)** — §1's owed observation, plus:
   with lanes visible, does the automation cluster's lit state read correctly at
   stage distance?
3. **Selection-ring strength (TIMELINE_UI_REDESIGN §H, marked OPEN there)** — the ring
   reads clearly on the multi-select render but thin at 1× on busy headers; §H
   anticipated "may push brighter/thicker for stage legibility."
4. **First-lane recording flow** — ARM, play, wiggle a knob: does the punch-in/out
   gesture feel like Live's overwrite? (P3 recording shipped without a feel-pass.)
5. **Muted-lane readability mid-set** — until spec item 3 ships, muted layers are
   only distinguishable by the M chip; confirm this is tolerable for the ~Aug release
   or pull item 3 forward.

## 4. What this branch changed (fixed-now items)

- `ui_snapshot/script.rs`: panel actions dispatch through the REAL `ui_bridge::dispatch`
  (was: a mirrored `LayerClicked` arm; everything else logged-and-dropped). Key events
  no longer discard their resolved actions. `UserPrefs::in_memory()` keeps D7
  determinism. This unblocks VD-002's driver-reach blocker (open-picker dispatch now
  possible headless) and makes every transport/inspector wiring headless-verifiable.
- `interaction_overlay.rs` + `app_render.rs`: D15 landing-line flash re-hooked at the
  Move-commit drag end; fires only when a move actually landed; unit test drives the
  full gesture. Feel sign-off owed (item 3.1).
- `scripts/ui-flows/toggle-lanes.json`: the LANES toggle proving script (both
  directions, asserts + PNGs).
- ui-snap `project:<path>` scene (worker, this branch): loads a real .manifold file
  for real-scale renders; plus an `empty` scene (zero layers, File→New state).
- ui-snap `--scroll` seed order (+ the header-bake re-sync) and structural
  interact-miss detection (this branch; details in §2.6).
- Layer-header name/gen-type label collision on indented child rows (found on the
  Liveschool real-scale render: "FLUID SIM 2D" and its type label drew over each
  other on group-child compact rows) — fixed by a worker on this branch,
  PNG-verified against the same render.

## 5. Verified-invariant evidence (things that are RIGHT)

- Single-source Y-layout holds under scroll: header column and lanes move together,
  no bleed into the ruler (states scene, scroll-seeded after-render).
- S1 multi-select chrome: per-clip crisp borders across a shift-range, no region band,
  no gaps (selectionclips driven render) — the P1.3b fix holds.
- §7 automation affordances that DID ship render correctly: lane strips + red
  breakpoint line, overridden lane grays, param-card red/gray dots, BACK lights on a
  latch (automation + inspector scenes).
- Layer-controls descriptor engine renders per-type cards correctly incl. the audio
  card (M/S/A + gain dB + send dropdown) — LAYER_CONTROLS shipped state confirmed.
- Hairline clips at 1px/beat stay individually visible with borders (hairlineclips).
