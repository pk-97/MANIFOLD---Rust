# UI reliability audit — 2026-09-06

<!-- index: Health-check coverage map for scene UI, editing, renderer updates, GPU lifetime and performance, with mapping repro probes. -->

Audited base: `84c5d672613a97f1762fd83c09f359e09f9237d8`. Author: Astra.
Scope: parameter projection, modifier card adaptation, graph-editor mapping,
input ownership, structural editing, scene update propagation, RT transitions,
GPU lifetime and selected performance paths. The first pass traced mappings;
the second broadened coverage across these classes. This remains a staged
health check with the explicit verification limits below. The initial audit
made no application changes. Astra owned diagnosis; one read-only Luna lane inventoried
interaction tests, whose relevant paths were then checked by Astra.

## First repair: mapping ownership and live values

Implemented in commits `725b148c7` and `c44ea3404`.
The final landing gate passed all seven required checks.
The findings below describe the audited baseline, not the repaired branch.
Card open actions now carry their target, parameter and clicked anchor. Both
mapping popovers capture a stable owner for every edit, and readers use the
live manifest. Stock and user bindings on effects and generators share canvas
eligibility. Mapping gestures retain their baseline for undo; dismissal cancels
unfinished scrubs and changing graph closes the old modal.

Native before/after check: with Bloom watched, Soft Focus Radius required two
clicks before the repair and one afterward. Changing maximum 64 → 128 persisted
through reopening the modal, undo (64), redo (128), save and project reload
(128). Focused checks passed: 31 UI mapping tests and 8 app mapping tests,
including repeated parameter ids, generator ownership, stale metadata,
cross-target gesture events, cancellation and one-entry undo. A real scene
modifier and nested canvas scopes still need native coverage; audio-row
alignment, editable modifier membership and cross-pane pointer capture remain
separate unfinished repairs.

Testing also exposed launcher defects: every launch generated another app
identity, and native macOS Quit skipped the crash-marker cleanup after
`run_app`. The launcher now reuses its worktree identity and refuses duplicate
instances. Cleanup also runs in the existing exit callback, and automation
markers live beside their socket instead of sharing the regular app marker.
Observed normal Quit removed the marker; relaunch reused the same approved
identity and rendered without a crash alert. The marker-path regression and
automation-feature clippy passed. Actual crash markers remain enabled.

The native modifier follow-up stopped when opening the generator browser
produced Compositor/Frame GPU address faults and a queue-blacklist exit. This
is recorded on existing `BUG-l7t4`; concurrent landing UI-flow checks mean an
uncontended reproduction is not established. The complete local log is
`/tmp/manifold-audit-generator-browser-gpu-fault.log`.

The first landing gate passed tests/clippy and 26 of 27 required flows. The
remaining modifier-trigger flow failed identically on unchanged main
`bffcc34a9`: it expected 45 global trigger buttons but found 52. The flow now
addresses the intended Flow parameter through a name supplied by the existing
shared row builder, retaining its drawer and undo assertions.

## Result

The shared parameter surface exists and modifier cards reuse it. The important
gaps are in the contracts around that surface: mapping entry still distinguishes
binding families, some readers use obsolete state, mapping actions omit their
owner, and filtering does not preserve every row-associated field. A new widget
system or a modifier-specific mapping modal would leave these causes intact.

Hard-coded modifier construction does not establish that applied mappings must
be immutable: the existing `EditParamMappingCommand` already addresses both
effect and generator instances. Declarative modifier authoring (`BUG-e3p6`) is
a separate feature from repairing editing of an applied modifier.

## Contract map

| Boundary | Shared path | Observed exception |
|---|---|---|
| Live parameter description | `PresetInstance.params` → `param_surface` → `ParamRow` | Editor reshape readers still read graph `preset_metadata.params` |
| Card identity | Ordinary parameter edits carry target + parameter id | Mapping opener carries only parameter id and consults the watched graph |
| Mapping entry | Shared `MappingPopover` and `EditParamMappingCommand` | Canvas resolver accepts only effect user-added bindings |
| Modifier projection | Generator surface filtered into modifier cards | Audio rows remain an unfiltered positional side array |
| Modifier membership | Existing modifier kind/node identity is available | Card rows selected by editable section display text |

## Findings, in repair order

### 1. Mapping ownership is missing at the action boundary — BUG-ngyu (P1)

Static evidence; full mouse reproduction pending. `OpenCardMapping(ParamId)` in
`manifold-ui/src/panels/actions.rs:753` omits the owner. In
`manifold-app/src/app_render.rs:1967`, the modal seed resolves against the
currently watched graph before the clicked effect retargets it at line 2055.
Modifier clicks do not retarget (`panels/inspector/routing.rs:369`). Anchor
lookup also searches every card by parameter id alone (`inspector/mod.rs:547`).
Mapping writes subsequently consult the mutable watched target.

This permits a missed first click or resolution against another instance when
ids coincide. Carry the owning target through opening, geometry lookup, modal
state and commit. Verify two effects sharing an id, a modifier after an effect
is watched, and selection changes during an open mapping gesture.

### 2. Mapping readers bypass live parameter authority — BUG-rtt9 (P1, existing)

Executable seam proof. `editor_bridge.rs:115` reads graph metadata;
`watched_full_reshape` and the range-commit check use it. The production edit
command writes only the manifest (`manifold-editing/src/commands/effects.rs:1161`).
A probe changed maximum 1 → 7: the manifest held 7, the editor reader returned
1, and the commit comparison reported no change. This confirms the existing
issue rather than discovering a new one. The full application undo sequence
was not exercised.

Use manifest specs for live range/label/curve/invert and graph bindings for
their affine coefficients. Include `binding_for_node_param`'s inverse-mapping
read at `editor_bridge.rs:199` in the same repair. Verify reopening, successive
drags, one undo entry per drag, save/reload and calibrated node-face editing.
Related existing issues: `BUG-2b0`, `BUG-3ef`, `BUG-9u2`.

### 3. Canvas mapping eligibility differs by family — BUG-b1qr (P2)

Executable seam proof. `resolve_canvas_binding` at `editor_bridge.rs:442`
rejects generator targets, then restricts effect lookup to user-added bindings.
Equivalent fixtures resolved as: effect user binding **yes**, generator user
binding **no**, stock effect binding **no**. Scene exposures are stamped
`user_added: false` (`manifold-core/src/scene_exposure.rs:255`).

Reuse one target-aware binding resolver for both card and canvas entry. Verify
effect/generator × stock/user binding coverage, nested scopes and a real modal
interaction. Do not change binding provenance merely to bypass the UI gate.

### 4. Modifier filtering loses audio-row alignment — BUG-tena (P2)

Static producer/consumer mismatch; runtime reproduction pending.
`projection/cards.rs:554` filters parameter rows, but line 599 copies the full
audio state. `param_card/render.rs:169` feeds that state to
`param_slider_shared/state.rs:446`, which reads `audio.rows[i]` positionally.
A later manifest row therefore receives a prefix row's audio presentation.

Preserve audio state with the selected row identity. Verify distinct sends,
trim ranges and armed states with generator rows preceding two modifiers.
Audit other filtered surface adapters for the same mismatch.

### 5. Editable section labels determine modifier ownership — BUG-xl2w (P2)

Static contract conflict; runtime reproduction pending. `modifier_surfaces`
selects rows by `spec.section == descriptor.display_name`; the shared mapping
modal allows section edits. Renaming or clearing that field removes the row
from this modifier projection. Hidden generator-card rows may then have no
card home. Separate stable ownership from editable display grouping. Verify
section changes, undo and reload preserve control reachability.

## Verification and reproducibility

Two temporary Rust probes called production functions in an isolated warm
worktree at the audited base. Compilation passed after correcting two probe
imports. The probes intentionally asserted the desired contract and both failed:

```text
AUDIT: before max=1, live manifest max=7, editor reader max=1; range commit detects change=false
AUDIT: effect user=true, generator user=false, effect stock=false
test result: FAILED. 0 passed; 2 failed; 0 ignored; 338 filtered out
```

The positive effect/user-binding control passed inside the second probe.
These establish reader/resolver behaviour, not rendered UX or complete gesture
lifecycle coverage. No GPU, full suite or performance checks were run.

The [probe patch](audit-evidence/2026-09-06-ui-contract-probes.patch) is retained
as reproducible audit evidence, not applied or ignored tests. In an isolated
slot at the audited base, apply the patch and run the repository build-lock
wrapper with:

```sh
cargo test --manifest-path /absolute/slot/Cargo.toml -p manifold-app --bin manifold audit_ -- --nocapture
```

Reverse the patch afterward. The audit worktree's production source was restored
after the run. Reuse these cases when implementing the fixes, extending them
through the actual input and command/undo path.

## Broader pass: coverage and root classes

The unit of repair is a shared contract with representative behaviours from
several surfaces. Mapping is one acceptance example, not the audit boundary.

| Class | Current evidence | Remaining acceptance |
|---|---|---|
| Control identity and live state | Two mapping probes fail; findings 1–3 above | Same edit through effect, generator, modifier and node face; correct target, displayed range and undo |
| Input ownership | Main UIRoot has captured `DragOwner`; editor routes ordinary canvas/panel events by current location | Cross-pane release, focus loss, Escape, text commit/cancel, popup transitions and first-click behaviour in a real editor window |
| Structural editing/persistence | All 13 `scene_modifier_inv_gate` tests pass, covering loop/fog apply/remove, modulation pruning/restoration, migration and duplication | User-edited sections/mappings, interleaved operations, saved-project roundtrip, and other modifier kinds when landed |
| Scene update propagation | Renderer applies graph value changes on version mismatch; coupled UI writes omit that bump; coupling exists on UI paths | Equivalent results through drag, type-in, undo, driver, envelope, OSC/Ableton and playback |
| RT mode/history transitions | Readiness, forced outputs and history resets are distinct mechanisms; existing tests exercise subsets | On/off/on identity, term-order independence, mode changes during playback and host rebuilds |
| GPU lifetime | Denoiser replacement remains immediate; GPU retirement infrastructure exists elsewhere | Resize/mode switch with frames in flight on classic and MTL4 paths; no crash or stale resource use |
| Runtime cost | Event-based generator eviction and texture-pool pruning exist; per-frame ID allocation and synchronous generator fusion remain | Measured allocation/frame-time distributions for static, animated, edited and deleted scenes |

Only **Scene Loop and Scene Fog** are registered modifier kinds on the audited
main tip (`scene_modifier.rs:169–173`). Mirror/Merge work mentioned by beads
and other worktrees was not part of this checkout and was not audited. The
green lifecycle result must not be generalized to unlanded implementations.

### Input ownership gap — BUG-ui2p (P2, new static finding)

`window_input.rs:936` always sends moves to the canvas; lines 955–967 send
moves to the editor UITree only over a panel or while the picker is open.
Release at lines 1484–1498 goes only to the surface under the cursor. The
originating surface can consequently miss its terminal event after a cross-pane
drag. `GraphCanvas::on_left_button_up` is where its drag session releases
(`graph_canvas/interaction.rs:1248`). This needs a native mouse reproduction.

The existing reuse target is `ui_root/drag.rs`: it captures the owner and
broadcasts terminal cleanup. Core `input.rs` tests correctly prove release
after widget removal and tree rebuild; they cannot compensate for a host that
never delivers release. Viewport/gizmo code already has unconditional release
cleanup, another useful comparison within the editor host.

### Write semantics and invalidation — existing BUG-agkv / BUG-6dh6

`generator_renderer.rs:650` applies inner overrides only after a graph version
change. `ui_bridge/project.rs:1319` writes coupled secondary node values without
bumping that version. This source mismatch agrees with BUG-agkv's prior runtime
reproduction; that reproduction was not rerun here. Another workstream was
already investigating it, so this audit does not duplicate its fix.

Coupling is resolved in the app's UI bridge while modulation writes the live
manifest separately. A correct direct drag therefore does not prove equivalent
modulation behaviour. Repair acceptance must cover all write sources and one
atomic update of dependent values, while preserving value-only invalidation.

### GPU lifetime and transition coverage — existing BUG-rnnr / BUG-zw2l

`render_scene.rs:2461` still drops/replaces the denoiser on dimension mismatch;
`denoiser.rs` has no retirement hook. `GpuDevice::retire_after_queue` exists for
objects indirectly referenced by GPU work, but the correct lifetime boundary
must account for classic and MTL4 queues. This is a source-supported priority
for reproduction, not a newly reproduced crash or proof about Apple's internal
resource retention.

The representative GPU toggle tests serialize frames with
`commit_and_wait_completed`. The performance-named
`temporal_upscale_toggle_never_stalls_past_20ms` even alternates two separately
constructed runtimes (`rt_t2b_temporal_wiring.rs:347`), rather than resizing one
live instance. These tests have value, but cannot establish the missing
in-flight transition guarantee. `rt_bugmajv_kernel_toggle.rs` explicitly records
that its pooled-resource lifecycle did not distinguish pre-fix source. The
ContentThread-based `rt-capture` harness is the existing next verification seam.

### Performance opportunities — BUG-ax9h and existing BUG-wj73

`generator_renderer.rs:606` allocates/clones a layer-ID vector every frame and
then linearly searches project layers for each entry. Reused scratch and a
dirty-built ID lookup are plausible small improvements; no speedup is measured.
Deleted generator states are already evicted on structural changes, and the
GPU texture pool has resolution eviction and age-based pruning. A blanket
claim that deleted scene resources are never released would be incorrect.

Generator fusion still compiles on cache miss
(`freeze/install.rs:416`, called from `generators/registry.rs:269`), while effect
fusion has a worker-backed ready/pending path. Existing BUG-wj73 covers this
asymmetry. Measure edit/start tail latency before changing the swap-in lifecycle.

## Broader-pass verification results

- `cargo test -p manifold-renderer --test scene_modifier_inv_gate -- --nocapture`:
  **13 passed, 0 failed, 0 ignored** on the audited base, using the build lock.
- Fresh worktree `manifold-app --features perf-soak` build succeeded. Two
  `rt_toggle_matrix.py` cells (`rt_enabled`, `sun-intensity-snap`) ran against
  `RtNoiseTesting.manifold` at 1280×720 with a **45-second limit per cell**.
  Both timed out: **transition verdicts inconclusive**. A frame-30 composite
  from the first cell was inspected and showed the fixture car rendering.
  This does not establish transition correctness or a performance baseline.
- The matrix tool discarded partial timeout stdout and then reported missing
  stats / an inert toggle from absent data. BUG-5v9d tracks preserving partial
  output and separating process failure from behavioural verdicts. The audit
  does not count those messages as confirmed rendering defects.
- Native probe artifacts/logs are local under
  `/tmp/manifold-audit-rt-20260906`; lifecycle/build/matrix logs are
  `/tmp/manifold-audit-modifier-invariants.log`,
  `/tmp/manifold-audit-rt-build.log`, `/tmp/manifold-audit-rt-matrix.log`.
  The build was **dev**, not a release performance measurement.

The interaction inventory confirmed that `ui-snapshot` drives one headless
UIRoot, handles undo specially, and rejects `AutomationAction::Text`
(`ui_snapshot/script.rs:493–586`). Registered scene flows and focus/drag unit
tests therefore do not establish native editor text/modal ownership. The new
`scripts/live_ui.py` and `launch_live_ui.py` provide an existing isolated-app
entry for closing that gap; no additional automation framework is needed.

## Repair order and next checkpoint

1. Establish live input reproductions for ownership and cross-pane drag
   termination, then repair target identity and live-state authority together.
   Use effects and scene controls as acceptance cases in the same change class.
2. Verify equivalent scene updates across direct and modulated writes, coordinate
   with BUG-agkv's existing work, and retain the passing lifecycle contracts.
3. Validate RT history/mode transitions and GPU retirement with the actual host
   lifecycle before applying performance changes. The bounded attempts above
   need a smaller or warmed discriminating fixture and preserved process logs.
4. Measure performance on static, animated, edited and removed scenes, then
   address attributable allocations/rebuilds/compilation. No free-upgrade or
   whole-engine health claim is supported yet.

The next checkpoint should include observed interactions and completed native
transition verdicts across these classes, not just more mapping-specific fixes.
