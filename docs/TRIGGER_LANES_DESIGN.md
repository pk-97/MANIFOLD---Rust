# Trigger lanes — independent clip patterns for parameter modulation

<!-- index: Child trigger lanes with no thumbnails; shared assignment from lane headers and parameter drawers. Current-code audit, proposed architecture, and first-slice acceptance contract. -->

**Status:** IN PROGRESS · 2026-10-09 · Codex. Source persistence, undoable
assignment, shared active-source timing, arrangement/session intervals and typed
clip-event delivery are implemented. Trigger-lane ownership, media exclusion,
authoring UI and full runtime acceptance remain pending.
**Tracking:** `BUG-tqtel` (feature).
**Prerequisites:** crate refactor landed; reverify the audited seams against subsequent cleanup.
**Execution contract:** read `DESIGN_DOC_STANDARD.md` sections 5–6 before briefing
implementation. Section 6 below names the remaining entry blockers. This document
completes the initial audit and behaviour contract, not feature implementation.

Peter's foundation requirement: “ensure our base level engine systems, contracts,
APIs, and Interfaces are unified for these cross domain systems” and “Unified
simple SOLID like systems.” Section 4 makes this a prerequisite of the feature:
consolidate the related authorities, rather than adding trigger-specific copies.

The performer draws a pattern once and assigns it to parameters. Main-lane clips
remain the default timing source; optional child trigger lanes supply independent
patterns. A scene is still one running scene, with independently controlled forces.

Peter: “the header of the trigger lane should also have a dropdown or nested menu
or something that gives it 2 way coupling for assignment there also.” The header
and modulation drawer edit **one connection**, not two routing configurations.
Peter: “these trigger lanes don't need thumbnails either.” No thumbnail production,
cache entries, or preview rendering should be requested for them.

Companions: `CORE_ENGINE_MAP.md` (playback authority), `WIDGET_TREE_DESIGN.md`
(shared parameter surface), `PARAM_STEP_ACTIONS_DESIGN.md` (existing responses),
`AUDIO_MODULATION_DESIGN.md` (audio sources), `AUTOMATION_LANES_DESIGN.md`
(continuous timeline values), `DESIRED_STATE_RECONCILIATION_DESIGN.md` (mute and
membership), `ABLETON_SHOW_SYNC_DESIGN.md` (import policy, not changed here).

## 1. Audit — what exists

Verified 2026-10-09 at `f6da1267233aa824634d4ddd9c265cee3d862874`. Source inspection
only: no app, runtime timing, visual, or performance verification was performed.
Symbols are the durable anchors; line numbers are navigation hints. Extend these
mechanisms rather than inventing a second timeline or parameter-address system.

| Piece | Current source anchor | Finding |
|---|---|---|
| Layer and clip kinds | `crates/manifold-core/src/types.rs:116` (`LayerType`, `clip_kind`, `accepts_clips_from`) | Exists: Video, Generator, Group, Audio, Dmx. Groups accept no clips. A trigger kind is new. |
| Layer hierarchy | `crates/manifold-core/src/timeline.rs:216` (`enforce_tree_order`); `crates/manifold-core/src/layer.rs` (`parent_layer_id`) | Exists: a flat, ordered parent forest using stable `LayerId`. The sorter itself is generic, not restricted to groups. Presentation and editing still carry group-specific assumptions. |
| Clip editing and ownership | `crates/manifold-editing/src/service.rs` (`create_clip_at_position`, `duplicate_layers`); `crates/manifold-editing/src/commands/layer.rs` (`DeleteLayerCommand`); `crates/manifold-core/src/layer.rs` (`enforce_non_overlap_for`, `clone_with_new_ids`) | Exists: clip gestures, overlap enforcement, fresh IDs and undo. Deleting a layer currently detaches children; trigger-child ownership needs an explicit rule. |
| Playback membership | `crates/manifold-playback/src/engine.rs:1595` (`sync_clips_to_time`); `crates/manifold-playback/src/scheduler.rs` (`ActiveClipRef`, `compute_sync`) | Exists: timeline/live/session reconciliation. Start edges are accumulated by numeric layer index; drag heals suppress firing. |
| Media-independent activation | `crates/manifold-playback/src/engine.rs:1355` (`start_clip_with_edge`, `stop_clip`) | Missing: active membership currently follows successful renderer start. Trigger clips need activation without a renderer. A dummy generator is the wrong fix. |
| Parameter envelopes | `crates/manifold-core/src/effects/envelope.rs:30` (`ParamEnvelope`); `crates/manifold-playback/src/modulation.rs` (`apply_instance_envelopes`, `compute_active_clip_timing`) | Exists: decay, step, random. Timing currently scans visible arrangement clips per layer and rising edges are inferred from activity/elapsed time. This is separate from scheduler edges and does not enumerate live/session refs. |
| Audio and named Fire | `crates/manifold-core/src/audio_mod.rs` (`ParameterAudioMod`, `TriggerAction`); `crates/manifold-playback/src/modulation.rs:715` (`fire_parameter_clip_edges`); `crates/manifold-app/src/ui_bridge/dispatch/modulation.rs` (`AudioModToggle` arm) | Exists: per-param audio sources and clip-edge Fire delivery. Clip-only Fire already works without a send, represented by an audio-mod record with an unassigned send ID. Reuse its behaviour; separate the source/response contract before adding lane selection. |
| Retained composition | `crates/manifold-playback/src/modulation/composition.rs` (`ControlSample`, `compose_controls`, `compose_param`) | Exists: shared frame/hop composition. `ControlSample.active_elapsed` is instance-wide; independently sourced parameters require parameter-specific timing here too. |
| Targeted delivery versus broadcast | `crates/manifold-app/src/content_pipeline/trigger_targets.rs` (`accepts`, `scene_impulse`); `crates/manifold-app/src/content_pipeline.rs` (`apply_trigger_pulses`); `crates/manifold-compositor/src/generator_renderer.rs` (`route_audio_pulse`) | Exists: retained owner/parameter validation and named scene impulses. Legacy host/effect Gate pulses can still increment a layer counter. They are not equivalent to isolated per-parameter Fire. |
| Parameter identity and persistence | `crates/manifold-core/src/params.rs:36` (`Param`); `crates/manifold-core/src/effects/instance_serde.rs` (`ParamEntryWire`, `from_param`, `apply_to`); `crates/manifold-editing/src/commands/effect_target.rs` (`DriverTarget`) | Exists: manifest-backed identity, stable effect/layer targets, one parameter wire entry. No source-lane field exists. |
| Header and drawer | `crates/manifold-ui/src/panels/layer_header.rs` (`compute_layer_row`, `LayerHeaderPanel::build`); `crates/manifold-ui/src/param_surface.rs` (`ParamRow`, `RowRole`); `crates/manifold-app/src/ui_bridge/projection/timeline.rs` | Exists: reusable row geometry, hierarchy projection and shared parameter surfaces. Header target picker and drawer lane-source selector are new affordances. |
| Thumbnail pipeline | `crates/manifold-app/src/ui_frame.rs` (`ThumbPass`, thumbnail pass); `crates/manifold-app/src/content_pipeline.rs` (`set_clip_atlas_visible`, parked thumbnail production) | Exists separately from clip rectangles. Merely omitting an image from the trigger row is insufficient; eligibility must exclude trigger clips before requests and rendering. |
| Physics control invalidation | `crates/manifold-node-engine/src/water/runtime/physics_source_controls.rs` (`digest`, `hash_envelopes`, `hash_audio_mods`) | Exists: authored control hashing is separate from runtime accumulators. A new source field and its authored pattern dependencies must participate in preparation invalidation where relevant, not merely save/load. |
| Export and scripted UI | `crates/manifold-app/src/content_export.rs` (`engine.tick` frame loop and warmup); `crates/manifold-app/src/ui_snapshot/script.rs` (`evaluate_modulation` call) | Export already uses engine playback. The scripted UI invokes modulation directly and must consume the shared timing contract too; otherwise a passing UI demo could exercise a different path. |

History check: `e2f56beca` introduced engine clip edges for parameter steps;
`53b00fc66` made layer-rebinding heals suppress edges. Preserve the latter: moving
an active clip is not a musical trigger. Re-run `git log -S 'clip_edge_layers' --
crates/manifold-playback/src/engine.rs` when preparing that seam.

Two corrections to the early discussion: generic parent ordering already exists,
and parent mute is not universally a modulation stop. `evaluate_all_envelopes`
explicitly permits modulation on muted layers. Neither grouping nor mute can be
changed by substituting a new layer-kind flag everywhere.

## 2. Decisions

**D1 — Agreed interaction.** Child trigger lanes work beneath video, generator,
audio, DMX and group layers. They contain ordinary editable timing clips, with no
per-clip target assignments and no thumbnails. “All layer types” means a common
mechanism for their compatible parameters, not an assertion that every mixer
control already belongs to the parameter manifest.

**D2 — One connection, two editing surfaces.** A lane may drive several parameters;
each parameter chooses one clip timing source, defaulting to its own main lane.
Choosing a child replaces that parameter's main-lane timing, not its independent
audio input. Header selection and drawer selection write the same authored state
in one undoable command. The header target list is derived, never separately saved.
Rejected: target lists on both lane and parameter, because they can disagree.

**D3 — Recommended storage: reuse layers and clips.** Add a non-rendering Trigger
kind to `LayerType` and `ClipKind`, retaining `LayerId`, `TimelineClip`, `ClipId`
and the existing overlap/gesture infrastructure. A trigger layer must have one
non-trigger owner and is a leaf. Its `parent_layer_id` expresses that ownership;
ordinary content parenting remains group-based. Do not turn the owner into a
compositing group. Groups may own trigger children beside their content children.

Rejected: nested `Layer.trigger_lanes` with a separate clip container. It looks
smaller at first but requires another lookup, editing, selection, live/session and
serialization path. Rejected: an empty generator as a trigger lane, because media
lifetime, thumbnails and render readiness must not determine musical events.
Consequences, stated honestly: a new kind requires exhaustive checks across media
admission, MIDI/session launching, rendering, thumbnails and layer operations.
It is not just an extra timeline row.

**D4 — Recommended routing scope.** A regular owner's trigger children may target
that owner's compatible parameters. A group's trigger children may target the
group and its descendant content layers. The drawer offers local trigger children
and trigger children of ancestor groups. Unrestricted cross-project routing is
outside the initial feature. A main lane remains only its own implicit source;
this proposal does not turn every unrelated media lane into a routing source.

**D5 — Preserve content lifetime.** Trigger children do not launch, restart or
reset their parent content. They follow transport independently of parent clip
presence, which also supports clipless groups. An unavailable target receives no
queued burst when it later becomes available. Source switching while a clip is
already active does not synthesize a new Fire; continuous responses may adopt its
current phase. Explicit scene reset is a separate parameter action.

**D6 — Agreed mute policy.** Peter: “Keep child triggers running.” Muting a trigger lane or one of
its clips suppresses its outgoing contribution. Unmuting resumes current phase,
without replaying missed impulses. Parent/group mute and solo keep their existing
media/presentation semantics; they do not implicitly turn into trigger-lane mutes.
This preserves evolving muted scenes. The UI must distinguish source mute from
target visibility. Parent mute must not silence its trigger children.

**D7 — Keep response editing on the parameter.** First implement clip-start timing
for existing decay/step/random responses and named Fire targets. Do not silently
reinterpret a legacy broadcast Gate as a targeted Fire. Duration-based enabling
of audio/LFO routes and clip-progress mapping are later behaviour work, not implied
by source selection. Existing direct audio/transient modulation remains available.

## 3. UI contract

```text
v Particle Scene
    Hits       [Radial Fire +1 v]   | | | |
    Swells     [Vortex Strength v]  =====  =====
```

The parent exposes **Add trigger lane**; the drawer exposes **Create trigger lane**
with its parameter preassigned. New unassigned headers show **Assign…**. The
header picker is searchable and grouped by child layer, effect/scene item and
parameter, with checked targets. One pattern can check several targets. Selecting
a target already assigned elsewhere moves that connection, retaining its response.

The drawer shows **Trigger source: Main lane / named child lane**. Groups have no
implicit main clip source: show **No clip source** until explicitly assigned.
Removing a header check disconnects that lane and disables the clip response;
it must not unexpectedly return the parameter to firing on main-lane clips.
The drawer can explicitly choose Main lane to restore that behaviour.

The first target plus a count fits the header. The picker shows all targets and
offers navigation to each drawer. Selecting a drawer source reveals/highlights its
lane. Response controls stay in the existing shared drawer, not in a second header
inspector. Collapsing child rows affects layout only, never playback.

Trigger clips reuse draw, move, resize, duplicate, delete and quantization gestures.
Display solid blocks and start markers; no media name, waveform, preview or thumbnail
requests. The first slice must include creation from both entry points, assignment
in both directions and undo. They are not post-launch polish.

## 4. Architecture boundary and remaining seam work

### One responsibility per boundary

The shared contract is **source timing → destination response → typed delivery**.
Unification means one authoritative answer to each question below, not one giant
manager, universal event bus, or common float standing for both values and events.

| Responsibility | Authority and reuse | Required consolidation |
|---|---|---|
| Which clip events happened, and which source is active? | Playback reconciliation, its timeline/live/session inputs, stable layer/clip IDs | One read-only clip-control view supplies ordered starts and sampled elapsed/duration to every consumer. Remove modulation's independent arrangement scan and local inference of source starts once callers migrate. |
| What time does this input belong to? | `Beats`, `Seconds`, `AudioHopStamp`, engine input epochs and existing tempo conversion | Preserve clip beat identity and audio sample stamps. Convert at the established transport/simulation boundary; never stamp every event with the current render frame or discard equal-time distinct events. |
| Which destination and response is selected? | Manifest parameter identity, `GraphTarget`/`DriverTarget`, existing response records | One authored clip-source selection, one validation/editing operation, two UI projections. Source choice is independent of whether an audio send exists. Do not add a second route table to the lane header. |
| What is the effective value? | `modulation/composition.rs` (`compose_controls`, `compose_param`) | One pure composition implementation receives per-parameter source samples. Snapshot-only input, retained audio hops and export must not acquire different trigger-lane arithmetic. Input advancement happens once before sampling; sampling never fires a second event. |
| What action reaches a subsystem? | Existing `TriggerPulse`, retained delivery queue, `TriggerTargets`, named scene impulse producer | Extend typed, destination-addressed delivery. Preserve the distinction between a named Fire and a whole-preset Gate. Manual, clip and audio Fire converge on the same destination operation. |
| Does this clip need media resources? | Playback activation and explicit layer/clip capabilities | Scheduling membership is independent of renderer acquisition. Video/audio/generator consumers own readiness and resources; trigger clips own none. Avoid scattered “not audio means generator” fallthroughs. |
| What invalidates prepared state? | Existing manifest reconcile, project edit versions and physics control digest | Include authored routing and applicable source-pattern dependencies. Keep transient counters, meters and event cursors out of serialization and authored hashes. |

Concrete duplicate paths found: `evaluate_modulation` selects staged composition
when `audio.hop_batches` is empty and pure retained composition otherwise;
`apply_instance_envelopes` still composes decay separately from `compose_param`.
The two paths have documented shadow-update timing differences. Characterize those
before consolidation; do not silently change existing show timing. A single pure
composer can serve both while input-advancement policy remains explicit.

The timing migration now builds each retained hop's `ControlSample` at the hop's
timestamp and tempo-derived beat. Arrangement spans cover the interval since the
last evaluation, including session loops and quantized replacement/stop boundaries.
Ended live-note and Back to Arrangement transitions still need the same guarantee.

Small typed interfaces should expose only what their consumers need: clip events
and phase to modulation, evaluated controls/events to rendering, and snapshots plus
edit intents to UI. Do not make the node engine depend on the UI or have GPU code
resolve timeline rows. Implement source selection once; layer kind affects available
targets and media capabilities, not a separate modulation evaluator for each kind.

Production caller inventory, excluding test modules, verified at the audit commit:

| Seam | Callers to migrate together |
|---|---|
| `evaluate_modulation` | 3: engine playing/non-playing ticks, app `ui_snapshot/script.rs`. Export reaches it through engine ticks, not a fourth evaluator. |
| `evaluate_all_envelopes`, `evaluate_all_audio_mods` | 1 each: staged branch of `evaluate_modulation`. |
| `compose_controls` | 1: `compose_instance_retained_controls`. |
| `compose_param` | 2: `compose_controls`, `record_hop_values`. |
| `ControlSample` construction | 3: base sample plus effect/generator timing overrides in `compose_retained_controls`. |
| `compute_active_clip_timing` | 2: retained and staged branches of `evaluate_modulation`. |
| `pending_clip_edge_layers` | No production consumer; test instrumentation must migrate with the edge representation. |
| `TriggerPulseKind::Gate` consumption | One app delivery branch in `apply_trigger_pulses`; layer/modifier route and master counter are distinct destination semantics. |

Re-derive before changing these APIs:

```sh
rg -n 'evaluate_modulation|evaluate_all_envelopes|evaluate_all_audio_mods|compose_controls|compose_param|ControlSample|compute_active_clip_timing|pending_clip_edge_layers|TriggerPulseKind::Gate' crates --glob '*.rs'
```

Classify tests separately and read enclosing functions; raw text-hit counts include
comments, imports and test code. A new production caller changes the seam brief.

**Implemented timing contract (2026-10-09).** `ClipControlFrame` is keyed by stable
`LayerId` and exposes `elapsed(source, owner, beat)` and ordered `starts(source, owner)`.
The scheduler records logical membership from the same timeline/live/session refs
used by `sync_clips_to_time`, before renderer acquisition or its warm-up guard.
Rebinding a clip remains silent; session iteration changes emit starts. Main clip
mute is carried with the event so envelopes preserve their mute behavior while
legacy audio clip-edge responses remain compatible. Parent mute does not gate it.

`evaluate_modulation`, `evaluate_all_envelopes` and `evaluate_all_audio_mods` consume
that frame. `compose_controls` receives a per-parameter sample. The independent
arrangement scanner, numeric-layer edge buffer and envelope edge-inference fields
are removed. Scripted UI steps use an engine with no content renderers; it verifies
UI/control state, not scene impulse delivery. Export already uses engine ticks.

Focused CPU checks cover source isolation, disabled/missing sources, adjacent starts,
renderer independence, mute, source deletion, seek cancellation and session launch.
Arrangement queries now use the existing dual sorted indexes for a beat window;
point queries delegate to equal endpoints. Media membership filters that same
result at the current beat. The scheduler also emits starts from clips wholly
crossed during forward playback, independent of media lifetime. A separate
evaluation boundary retains phase coverage across out-of-tick syncs; source
events use the last reconciled beat so those syncs cannot deliver a start twice.
Initial membership is reconciled before the first time advance. Explicit seeks
discard pending starts and traverse no skipped region; backwards clock movement
also uses destination membership only. A CPU engine proof compares the same short
pattern at fine/coarse display intervals, repeated syncs and fixed export time.

Session resolution now captures crossed iterations before applying pending launches
or stops. It retains completed spans across repeated syncs, closes arrangement
coverage at the first session launch and avoids firing arrangement clips when a
session launch starts transport. A bounded interval that cannot be retained latches
the existing delivery failure instead of publishing a partial Fire stream.

`TriggerSourceStamp` distinguishes snapshot, audio-hop and clip events. Clip Fire
events retain source layer, clip and beat separately from destination identity.
Scene delivery converts that beat using the project tempo map. The existing native
event queue already orders source timestamps and preserves equal-time events;
there is no new delivery queue. CPU proofs cover provenance and tempo conversion;
they do not establish rendered force isolation. Audio events now carry resolved
transport seconds beside their original hop stamp. Advancement settles the existing
hop clock once per new batch; parameter sampling reuses it, and replay cannot
re-anchor it. Delivery converts those source seconds to beats, independently of
the accepting display frame. Live phantom clips now retain their owning LayerId
from creation. Focused CPU tests cover source-clock/sample agreement, replay,
invalid-clock rejection and phantom identity; app compilation and clippy pass.
Rendered timing remains unverified.

Live NoteOff, replacement and one-shot expiry retain completed intervals until
modulation samples them. Source starts are delivered once even when an entire
note falls between syncs or begins at the previous sync boundary. NoteOff uses
the accepted raw event beat; recording quantization remains separate. Focused
CPU checks cover interval boundaries, repeated sync, existing MIDI guards and
seek cancellation.

Full acceptance below remains open: Back to Arrangement phase
coverage, external-clock discontinuities, audio clip-Step/Random multiplicity,
audio source-time verification, rendered scene timing and snapshot/retained composition
consolidation must be completed before exposing trigger lanes.

### Model, scheduling and delivery

Content-thread ownership, snapshots, `ContentCommand` and `EditingService` remain
unchanged. UI emits intents; it never edits project state. No new thread or locks.

The source choice belongs to the destination parameter's existing manifest state,
with a serde-defaulted field on its existing wire entry. Use stable `LayerId` for
explicit sources and existing effect/generator plus `ParamId` addressing for edits;
never row indices, labels, graph-node positions or synthesized scene addresses.
Source absence must preserve old projects; an explicit missing source must stay
inert and visible rather than fall back to main. Reconciliation, graph rebuilding,
preset copy and layer duplication must preserve/remap this field deliberately.

`sync_clips_to_time()` remains the membership authority. Trigger clips become active
without acquiring a renderer; filter them out before media readiness, compositor,
audio-voice, DMX and thumbnail paths. Keep the existing live/session inputs and
MIDI ordering/channel guards. Do not create a second scheduler in the UI or renderer.

Replace the layer-index-only edge input with stable source identity and explicit
event timing. The selected source must provide both start events and current
elapsed/duration to envelope, Fire and retained-hop composition. Cache lookup
structure on edits; reuse scratch storage, using `AHashMap` for hot ID lookup.
The runtime must retain event order and count; a boolean loses multiple starts.

The scheduler currently evaluates active membership and applies a media warm-up
guard. The implementation brief must resolve short clips crossed between ticks,
adjacent clips, custom loops, seeks and external-clock jumps before claiming every
trigger is delivered. Trigger events must not be dropped by a media readiness guard.
Do not repair this by independently scanning trigger clips from the renderer.

Route selected named Fire events into the existing retained target-validation and
scene-impulse path, using the destination's owner identity. Source identity does
not replace destination identity. No parent-wide counter increment for an isolated
force. Whole-preset Gate behaviour remains explicitly distinguished in the picker.

**Kill-pass of the small-change hypothesis:** only adding a source dropdown fails
against three inspected seams: renderer-dependent activation, arrangement-only
envelope timing, and instance-wide `ControlSample.active_elapsed`. All three must
be handled for the feature to be correct. This is static architectural evidence,
not a runtime reproduction.

## 5. Invariants and acceptance checks

These are required new checks, not existing passes. The execution brief must place
them in concrete test modules and flow files before implementation starts.

| Invariant | Required machine check |
|---|---|
| One mapping edited from both surfaces | `trigger_lane_assignment_two_way_undo`: header assign → drawer reflects it → drawer changes source → both headers reflect it → undo restores all views. |
| No implicit main trigger after disconnect | `trigger_lane_disconnect_is_inert`: uncheck a target and start both child and parent clips; neither fires that response until explicitly rearmed. |
| Selected source only | `trigger_lane_source_isolation`: interleave main/A/B starts; parameter A follows A, B follows B, and unassigned legacy parameters retain main timing. |
| Source identity survives edits | `trigger_lane_reorder_duplicate_delete`: reorder, duplicate a whole owner subtree, delete a source, undo; no cross-wiring or fallback. Invalid source remains visible and inert on load. |
| No media side effects | `trigger_lane_no_media_lifecycle`: renderer spies observe zero starts, stops, prewarms, thumbnail requests and parent restarts from trigger clips. |
| Time and event parity | `trigger_lane_event_sequence`: adjacent/short clips, loops, play/stop/seek, live/session events, tempo changes and offline export produce the specified ordered event sequence. Exact expectations are an entry blocker below. |
| Persistence | `trigger_lane_roundtrip`: real save/load, then play and assert responses; test old files without the field plus missing sources. Cover manifest reconcile and duplication. |
| One generic mechanism | `trigger_lane_owner_kinds`: table-driven Video/Generator/Audio/Dmx/Group owners and nested groups; only compatible parameters offered. |
| Force isolation | `trigger_lane_force_isolation`: two forces in one running scene receive different patterns; target-event counts and simulation reset count are asserted through the production path. |
| No new hot-path allocation | Reuse scheduler buffers; bounded `MANIFOLD_RENDER_TRACE=1` acceptance run with retained audio hops and routed triggers. Apply the existing content-thread timing gate. |
| Shared composition and clocks | `trigger_lane_sampling_parity`: sample the same authored event sequence at frame and audio-hop times, with and without audio batches, and through export; compare values and fire counts at matching transport positions. |
| Prepared-state invalidation | `trigger_lane_control_digest`: source reassignment and relevant pattern edits invalidate prepared physics controls; meter/counter changes do not. |

Retain existing regressions in `crates/manifold-playback/tests/param_step_clip_edge.rs`
(especially reorder and real save/reload), editing overlap tests, MIDI phantom
ordering/channel checks and the shared parameter gesture/undo flow.

## 6. Phasing — first slice and entry blockers

**P0 — this audit and behaviour contract.** Deliver the source map, interaction,
architecture recommendation, alternatives and acceptance requirements. Verification:
source/reference checks and documentation index/diff checks only. No runtime claim.

**P1 — one complete connection through the real UI.** Intended performer gesture:
create a trigger child under an existing generator, draw two clips, assign one
ordinary numeric parameter with its existing decay response, then change its source
from the drawer. Main-lane starts must no longer drive that response. Save, reload,
play, disconnect and undo. Creation and assignment work from either surface, with
no thumbnail requests. Use the generic owner model from the outset; the demo's one
generator is a fixture, not a generator-only implementation. Target L3 using the
existing UI flow harness plus a playback-value integration test.

**P1 is not ready to dispatch.** The lead must close these named entry blockers and
write its exact signatures, exhaustive call-site inventory, named test modules and
runnable commands under `DESIGN_DOC_STANDARD.md` sections 5–6:

1. **Resolved: mute policy (D6).** Owner mute does not suppress child control output.
2. **Lead: source/response persistence seam.** Pin the exact manifest field,
   disconnected versus implicit-main representation, and command payload. Specify
   atomic response creation from the header and clip-only Fire arming without an
   audio send. Header unassignment must have one unambiguous serialized meaning.
3. **Lead: timing seam.** Pin the scheduler's stable source events, short-clip and
   seek policy, loop/retrigger rules and per-parameter frame/hop sampling signatures.
   Inventory all callers before replacing `clip_edge_layers` or `active_elapsed`.
   The replacement must serve existing main-lane modulation as well as new trigger
   children; keeping the old independent timing path for existing layers fails the
   foundation requirement. Pin the snapshot/retained composition migration and its
   behavioural characterization checks at this same boundary.
4. **Lead: ownership lifecycle.** Pin deletion of trigger children with their owner,
   group ungrouping, source deletion, moving a target out of scope and subtree
   duplication/remapping. Orphan rescue must preserve data without turning a
   trigger lane into a playable media layer.

### Source persistence and assignment contract

`manifold-core::params::Param.clip_trigger_source: ClipTriggerSource` is the
single authored connection. `ClipTriggerSource` has `OwnLayer` (default),
`Disabled`, and `Lane { layer_id: LayerId }`. Its `clipTriggerSource` wire field
is omitted for `OwnLayer`; explicit variants use `{ "kind": "disabled" }` and
`{ "kind": "lane", "layerId": "…" }`. An unresolved ID stays unresolved,
never falling back to Main lane. Existing audio source selection is independent.
`ParamEntryWire::from_param` and `apply_to` preserve it through save/load,
template reconciliation and graph-manifest refresh.

`manifold-editing::commands::trigger_source::SetParamClipTriggerSourceCommand::new`
takes `(GraphTarget, ParamId, ClipTriggerSource)`. It captures the old source at
execution and restores it on undo, resolving through `Project::graph_target_owner_mut`.
Both UI surfaces will dispatch this command through `EditingService`; scene
modifier rows pass the owning manifest's public macro parameter ID. A missing
owner/parameter is inert. Source-scope validation and atomic response creation
belong to the later authoring operation, not a second persisted route list.

Focused checks for this seam: `cargo test -p manifold-core --lib trigger_source`
and `cargo test -p manifold-editing --lib commands::trigger_source`, with
`CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0` in the leased worktree. The landing gate
adds scoped clippy and reverse-dependency checks. These checks establish
persistence and undo only; they do not establish playback or UI behaviour.

**Completion after P1:** named Fire/scene-force isolation, legacy Gate handling,
all-owner/nested-group coverage, MIDI/live/session/export parity and lifecycle
hardening. These are required for full feature completion. Split into bounded
seam-based briefs once P1's inventory establishes cost; do not publish a family-by-
family plan that duplicates routing code. No feature-complete claim after P1 alone.

Before each app landing: scoped check/clippy and module tests, required UI flow and
GPU proofs selected by touched paths, then `scripts/land_branch.py`. One cargo
command at a time with `CARGO_BUILD_JOBS=4`; GPU execution through the shared queue.
No broad rendering sweep is justified by this proposal.

## 7. Decided — do not reopen

1. Main timing remains the default; optional child lanes provide independent patterns.
2. Targets are assigned per lane/parameter, never manually on every clip.
3. Header and modulation drawer edit one connection in both directions.
4. All existing owner layer types and groups are in the completed feature's scope.
5. Trigger lanes have no thumbnails and do not instantiate duplicate scenes.
6. Responses use the shared parameter surface and undoable content-thread edits.
7. Related engine timing, target, response and delivery contracts are unified before
   the feature is declared complete; no parallel trigger-only engine or composer.

## 8. Deferred

- **Route gating and clip-progress responses:** revisit after selected clip-start
  timing and Fire work end to end. Require explicit response/combination semantics;
  the early “enable bass modulation during this clip” example is not delivered by P1.
- **Multiple clip sources into one parameter:** revisit on a concrete need for
  merged rhythms; current recommendation is one selected clip source per parameter.
- **Global cross-owner routing, master routes, raw mixer controls outside the
  manifest:** revisit with a concrete performance case and target inventory.
- **Automatic conversion of imported/detected lanes:** revisit when import UX is
  requested. `ABLETON_SHOW_SYNC_DESIGN.md` D2/D10 currently prescribe existing
  entities/generator clips; this proposal does not silently amend that import contract.
- **Triggering parent content into existence:** revisit only if explicitly wanted;
  it changes simulation lifetime and group semantics.
