# UI reliability audit — 2026-09-06

<!-- index: Initial contract audit of scene modifier cards and graph-editor mappings, with executable probes and ranked findings. -->

Audited base: `84c5d672613a97f1762fd83c09f359e09f9237d8`. Author: Astra.
Scope: parameter projection, modifier card adaptation, graph-editor mapping
entry, mapping read/write authority, and relevant gesture dispatch. This is
the first bounded health-check checkpoint, not a completed UI or 3D runtime
audit. No application changes were landed.

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

## Next coverage

First reproduce cross-card mapping and modifier audio interactions through the
live input path, then repair identity and state authority together before
expanding mapping eligibility. This avoids enabling more controls on an
incorrect target/readback path. Existing gesture coverage work (`BUG-3v6`,
`BUG-3ef`) should be extended at that seam.

3D real-time/RT engine correctness and performance remain unreviewed. The first
runtime seam should compare direct edits with modulation of coupled modifier
parameters (`BUG-6dh6`), followed by scene invalidation/resource lifetime and
bounded frame-time measurements. Existing issue descriptions are leads, not
independent runtime verification from this audit.
