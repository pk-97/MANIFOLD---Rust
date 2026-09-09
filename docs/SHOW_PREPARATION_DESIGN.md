# Show preparation — load completely, play without first-use stalls

**Status:** PROPOSED · 2026-09-07 · Codex · architecture draft, not built or approved for implementation.
**Prerequisites:** reconcile the current warmup work and BUG-qh04 before implementation; no crash root cause is assumed.
**Execution contract:** read DESIGN_DOC_STANDARD.md sections 5–6. This draft commits the architectural direction; phase entry requires the specified seam inventory and implementation brief before code.

Peter's goal: “remove all first render stutters and lags for entire show files, even if they are hours long with hundreds of scenes and clips and videos.” Load duration is not a reason to abandon required preparation. **Prepare the entire show's dependencies before declaring it ready, and make every scheduled transition resident before it is due.** Preparation must survive resource eviction; merely rendering a scene once is insufficient.

Companions: [WARMUP_DESIGN.md](WARMUP_DESIGN.md) describes the current implementation; this proposal replaces its timeout escape and expands its coverage if approved. [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) governs GPU ownership and ordering. [GIG_RESILIENCE_DESIGN.md](GIG_RESILIENCE_DESIGN.md) owns process recovery. No existing shipped status is changed by this draft.

## 1. Audit — what exists (verified 2026-09-07)

This is a bounded source audit, not a runtime proof. Extend these owners and paths rather than create a second renderer or decoder service.

| Piece | Source anchor | Classification and implication |
|---|---|---|
| Project warmup coordinator | `crates/manifold-app/src/content_commands.rs:82` | Exists; content thread coordinates loading and publishes snapshots. |
| Time/frame caps and failure outcomes | `crates/manifold-core/src/warmup.rs:41` | Exists; defaults are 10 seconds/layer, 600 frames, 60 seconds total. Exhaustion can leave work unfinished. |
| Generator pre-roll | `crates/manifold-renderer/src/generator_renderer.rs:1325` | Exists; activates first clip, uses production runtime, renders synthetic contexts. Does not establish coverage of every clip-dependent asset or dynamic branch. |
| Effect topology preparation | `crates/manifold-renderer/src/layer_compositor.rs:881` | Exists; preserve existing topology enumeration, including group/master/LED paths. |
| Image preparation | `crates/manifold-media/src/image_renderer.rs:381` | Exists; synchronous decode, local byte cap, temporary clip activation. Needs explicit ownership and failure accounting. |
| Video lookahead | `crates/manifold-media/src/video_renderer.rs:425` | Exists; submits WarmOpen candidates. Opening a file is not proof the correct launch frame is ready. |
| Decoder workers | `crates/manifold-media/src/decode_scheduler.rs:111` | Exists; worker-affinity ownership and result channels. Extend this service; no new decoder pool. |
| Playback authority | `crates/manifold-playback/src/engine.rs:1411` | Exists; `sync_clips_to_time` remains authoritative. Preparation never introduces a competing timeline evaluator. |
| Cold-touch instrumentation | `crates/manifold-foundation/src/cold_touch.rs:1` | Exists; extend evidence to resource readiness and transition deadlines. |
| Whole-show dependency closure, durable readiness and admission schedule | Above coordinator/renderer seams | New behavior proposed here; exact cache reuse inventory is required before adding storage. |

The prior Corrosion probe reported zero counted cold touches; subsequent heavy-project GPU failures demonstrate that this counter alone does not establish safety. One user-reported no-crash run with warmup disabled supports investigating warmup; it does not prove GPU overload or identify the fault.

## 2. Decisions

**D1 — Completion replaces a total loading deadline.** Required work ends in ready, cancelled, or a named failure. Never mark incomplete preparation successful because a timer expired. Retain operation stall detection, memory limits and admission limits: these control safety, not how much of the show receives preparation.

**D2 — Prepared and resident are different states.** All show assets and variants must be resolved and prepared. Only the working set needed now, soon, or for armed launches must occupy expensive GPU surfaces and decoder sessions. A prepared item evicted from GPU memory remains prepared but ceases to be resident.

**D3 — Enumerate dependencies; exercise the production path.** Inventory every clip, scene, graph variant and output configuration. Deduplicate using existing asset, graph and pipeline identities. Run real construction/render paths for each distinct requirement. Neither hand-maintained feature lists nor rendering only the first clip provide complete coverage.

**D4 — One content-thread coordinator.** Content owns preparation state, renderer mutation, admissions and transport readiness. Existing CPU/decode workers perform their current work and return results. GPU work stays behind manifold-gpu and existing queue ownership. No new threads, channels or shared mutexes are approved by this proposal.

**D5 — Start conservatively, enforce dependencies.** Initially admit one heavy preparation operation at a time, including AS builds and large uploads; make subordinate submissions participate in the same admission accounting. CPU work uses bounded existing worker capacity. Serialization alone does not prove safety or repair an invalid GPU operation.

**D6 — Readiness is specific to the show and machine configuration.** Changes to content, assets, quality, outputs, resolution, tempo/rate assumptions or relevant runtime versions invalidate affected preparation. A generic persisted `ready=true` is forbidden.

**D7 — No silent quality reduction or missing content.** If the required concurrent working set or sustained workload exceeds the machine's capacity, identify the scene and constraint before performance. The operator may edit or explicitly choose an optimization; preparation must not quietly alter the show.

**Consequences, stated honestly:** initial preparation can take minutes or longer and disk caches consume space. This removes avoidable initialization stalls; it cannot make a scene render faster than the GPU permits or guarantee uninterrupted playback after storage, hardware or driver failure. Unlimited instant access to every frame of arbitrarily large media is not a credible bounded-memory promise.

## 3. Readiness contract and performer experience

Opening a show displays preparation progress with the current operation, completed/total requirements, memory pressure and actionable failures. Progress may discover more dependencies and increase its total; it must not falsely reach 100%. The editor remains available for inspection and repair; performance playback becomes available when readiness passes. Cancel leaves the show editable and explicitly unprepared.

“Show ready” means all referenced dependencies have been validated and prepared, configuration matches, no required work is unknown or failed, the initial working set is resident, and a feasible resource/streaming schedule exists for the full arrangement under the declared tempo/rate/output configuration. It is not a claim that every decoded frame is in RAM or that every future frame has been rendered.

| Action | Required behavior |
|---|---|
| Play the arrangement | Initial frames are ready; future transitions are prepared ahead of their deadlines. |
| Loop or scheduled jump | Prepare the destination and wrap boundary as part of the schedule. |
| Launch an armed scene | Keep its launch resources and first frames resident; launch at the requested musical boundary. |
| Jump to an unresident location | Prepare the destination before committing the jump. Show its readiness; do not freeze the render thread while loading. |
| Edit a resource-affecting setting | Invalidate only affected requirements and reprepare. Current valid output continues while staging the change where possible. |
| Export | Use the same dependency preparation, then advance offline only when each exact frame is available. Never encode placeholders as successful frames. |

For an unready live launch, retain the current scene and mark the requested launch pending for the next valid boundary after preparation. For an unexpected arrangement underrun, keep external time authoritative, report a readiness violation and retain last valid output until current content is available; this is a failure response, not a passing hitch-free result. Do not slow Ableton or silently shift the score.

Instant arbitrary launching across the entire show requires retaining the launch working set for every destination. If that does not fit, expose which destinations are armed and ready; never label all destinations instant-ready. This tradeoff is part of the proposed product contract for Peter to review.

## 4. Preparation pipeline and ownership

The existing content-command loading path becomes an incrementally pumped coordinator:

1. **Discover:** traverse all arrangement clips, session scenes, nested graphs, string/media parameters, effect variants, group/master chains and LED/output routes. Use production resolution and existing IDs. Record dependencies and their consumers so edits invalidate precisely.
2. **Validate:** resolve local source bytes, including cloud-backed files, inspect formats and verify required assets are accessible. Required failures stay visible; absence is not quiescence.
3. **Prepare reusable data:** compile required pipeline variants, parse meshes, decode images/HDRIs, initialize models, and prepare media metadata/launch positions. Reuse existing caches; audit their keys and eviction semantics before extending them.
4. **Exercise:** build/upload and render each distinct resource-affecting configuration through production paths. Wait for dependent async content and successful GPU completion. A new content revision forces dependent AS/material work to rebuild. Exercise correct output sizes and quality settings.
5. **Reset mutable state:** return trigger, simulation, feedback, temporal accumulation and video position to the production activation state while retaining reusable resources. Warmup must neither fire external output nor advance the actual show transport. Compare first live state against a fresh activation.
6. **Plan residency and admit playback:** compute transition working sets and measured preparation lead times, make the initial/armed sets resident, then publish readiness.

A bool “pending” cannot express this contract. Proposed runtime state vocabulary in `manifold-core::warmup` is `Unprepared`, `Preparing`, `Prepared`, `Resident`, `Failed`, and `Cancelled`. Per-show aggregate state is `Discovering`, `Preparing`, `Ready`, `Invalidated`, `Failed`, or `Cancelled`. These are runtime-only, not fields in saved projects. Existing typed IDs identify consumers; cache keys identify reusable products. Do not invent parallel scene IDs.

The app coordinator owns the requirement graph and aggregate status. Renderer/media owners retain actual resources. `ContentState` publishes compact immutable progress/readiness snapshots; commands use the existing channel. Worker and GPU results carry the relevant preparation generation so late results cannot make a replaced project ready.

**Seam constraint:** replace blocking whole-layer preparation with resumable work steps returning pending, complete or failed, plus completion ownership for submitted work. Each coordinator iteration polls completion, services connection heartbeats and commands, then admits eligible work. Ordinary commands stay in FIFO order in a content-owned deferred queue; never dequeue and append them to their channel tail. Cancel/shutdown are explicit control actions. No blocking GPU wait inside a step.

Exact Rust signatures and the full call-site inventory are intentionally required at phase entry: this is an architecture draft, not an implementation-ready API contract. Do not start a mechanical lane directly from this section.

## 5. Memory, video and long shows

Maintain three lifetimes using existing resource owners: reusable prepared products on disk, bounded CPU-side data, and active/upcoming GPU and decoder resources. Pin active, armed and in-flight resources. Evict only unpinned resources with no GPU/decoder users; release by completion, not elapsed time. Account for staging and scratch overlap as well as final resource sizes. Unknown size requires bounded inspection or an explicit failure, never zero-byte accounting.

Before performance, inspect the full timeline's overlapping resource intervals, including transition overlap, decoder preroll, temporary upload/AS scratch and armed destinations. Reserve headroom for UI, outputs and driver allocations. There is no universal magic memory percentage: phase validation must establish a conservative configurable policy using measured allocations and device guidance. If even one required active set cannot fit, report that scene as unsupported on the present configuration.

Video preparation checks every source and clip entry position, not just file time zero. Respect in-points, trims, loops, rates, direction, frame timestamps and color configuration using the existing playback time mapping. A ready launch requires the correct frame already decoded and transferable without synchronous setup; a warm file handle alone fails readiness.

Extend the existing decoder scheduler so warmed state is promoted into playback under its existing worker ownership rather than reopened on activation. Key reusable source metadata separately from clip-specific decoder position; simultaneous uses at different positions need independent sessions. Bound queued work, decoder count and decoded frame bytes, with explicit cancellation/close and stale-result handling.

Lookahead is deadline-driven. Convert beat distances through the existing tempo map and playback rate; use measured open/seek/decode/upload latency plus safety margin to start preparation early enough. Recompute on tempo changes and jumps. Prioritize current-frame delivery, then imminent transitions, armed launches and distant work. Restrict background GPU preparation to available frame slack; if an indivisible operation cannot fit that slack, retain its product from initial preparation or declare the residency plan infeasible. Starting a huge AS build “in the background” is not protection from GPU contention.

Disk preparation must remain useful after RAM/GPU eviction: cache decoded/preprocessed immutable data where an existing compatible cache exists. Never assume native AS objects or live simulation state can be serialized. Determine reconstructable versus must-retain resources per owner. Content hashes, transformation settings and schema/backend compatibility belong in cache identity; paths and modification times alone are insufficient. Writes are atomic, corruption causes visible rebuild, disk-full blocks readiness when the cache is necessary for the schedule.

Dynamic graphs require explicit resource-dependency declarations for branches not reached by representative pre-roll. Finite asset selections and pipeline variants must be enumerated. Data-dependent unbounded asset selection cannot receive an unconditional ready claim; it needs a declared supported set. Ordinary animation and ongoing simulation remain rendering work, not something preparation pretends to finish.

## 6. Failure handling and audit trail

Every operation records project/preparation generation, scene/layer/clip, asset or variant identity, operation stage, timings, estimated/actual bytes and GPU submission identity where applicable. Reuse timestamped session/crash reporting and the existing GPU incident diagnostics. Keep a bounded event ring and low-volume summaries in normal operation; do not synchronously write verbose diagnostics per frame.

A stall watchdog diagnoses lack of progress for an individual operation; it is separate from total load duration. Timeout means failed/unresponsive, never prepared. Stop new dependent submissions, retain in-flight resources, save the incident and use the established fatal GPU path when completion/device health is uncertain. Cancellation cannot cancel an already submitted Metal command buffer. A wedged native operation also needs the existing out-of-process resilience design; an in-thread timer cannot guarantee process recovery.

GPU errors, missing/corrupt media, cache failure, insufficient memory and unschedulable decode demand have distinct user-visible reasons. A failed preparation must remain repairable in the editor when the device is healthy. Debug bypass remains explicitly unprepared and cannot produce a ready badge.

## 7. Invariants and enforcement

Proposed test names below are deliverables, not existing passing checks.

| Invariant | Machine check |
|---|---|
| Every required variant accounted for | `show_prepare_dependency_closure`: fixtures with different assets on later clips, nested branches, output variants and session-only scenes; unknown dependencies block ready. |
| No timeout becomes success | `show_prepare_pending_never_ready`: arbitrarily advance fake time while pending and assert readiness stays false. |
| Correct lifetime and cancellation | `show_prepare_cancel_pins_inflight`: delayed completions, cancellation, generation replacement, FIFO commands and no premature recycling. |
| Warmup preserves first live state | `show_prepare_activation_parity`: compare triggers, simulation/feedback reset and produced values against fresh production activation. |
| Eviction does not hide first-use work | `show_prepare_evict_reactivate`: force small capacity, evict/reload repeatedly, assert preparation completes before activation. |
| Video launches show the right frame | `show_prepare_video_entry_parity`: timestamp/value assertions for nonzero trims, repeats, rates and simultaneous source uses. |
| Cache identity remains valid | `show_prepare_cache_invalidation`: modified same-path asset, settings change, corruption and interrupted-write round trips. |
| Active/in-flight memory and work stay bounded | `show_prepare_admission_limits`: synthetic hundreds-scene schedule and delayed workers; assert byte/job bounds and explicit impossible-set failure. |
| Entire show is covered | `show_prepare_long_show`: schedule all boundaries across a synthetic hours-long show; no skipped late clips, bounded residency independent of elapsed duration. |
| Stage delivery has no preparation hitch | `show_prepare_transition_trace`: release playback traces count cold touches, allocation/compile causes, decode underruns and missed transition deadlines. Zero cold touches alone is insufficient. |

Performance acceptance uses the configured frame period plus the repository's content-thread trace gate: any content-thread frame above 20 ms fails, and a stricter configured period still applies. Report maximums and every missed deadline, not just average FPS. Separate initialization overhead from sustained scene cost; both can prevent a show from being ready for the selected machine.

## 8. Phasing

Each phase requires a separate implementation brief with exact signatures, current call sites, migration/deletion gates and commands before coding. Re-derive with `rg -n 'prewarm_layer|WarmupBudget|WarmupOutcome|warmup_pending|compute_prewarm_candidates|WarmOpen' crates` and inspect definitions plus callers. Changes to public APIs must replace the old path, not add permanent wrappers. Workers may implement only the resulting decided briefs.

**P1 — Honest readiness, end to end.** Entry: reconcile BUG-qh04 and current local changes. Read back core warmup types, content command/state handling and existing progress UI. Deliver an explicit readiness/failure state, no elapsed-time success escape, generation invalidation, cancellation/FIFO handling and a resumable coordinator for a minimal real preparation path. Gate pending/cancel tests, focused app/core checks, scripted loading-state UI assertions. Performer gesture: open, cancel, then reopen; playback never claims ready early. L3 target. Do not remove caps before waits and failure handling are safe.

**P2 — Complete show coverage.** Entry: P1 coordinator and status work. Read back generator installation, graph dependency resolution and compositor enumeration. Deliver whole-show requirements, production-path pre-roll, asynchronous dependency closure, state reset and shared API migration for all preparation callers, including edit/export. Gate closure/parity tests and a held-out project with later-clip assets. Focused renderer/playback/app checks plus GPU proof gate. Produce readiness trace; L2 target. No first-clip substitution or representative frame claimed as exhaustive branch coverage.

**P3 — Bounded residency and reusable preparation.** Entry: P2 can identify every required product. Inventory actual caches, resource pins and rebuild paths before adding storage. Deliver accounting, heavy-work admission, completion-safe eviction, cache invalidation and a full-show feasibility schedule. Gate admission/eviction/cache round-trip tests and computed allocation traces; affected-crate clippy/tests plus GPU proofs. L2 target. No unbounded “cache everything,” guessed zero sizes, or native-resource serialization assumptions.

**P4 — Video deadlines and promotion.** Entry: P3 residency accounting. Read back decoder scheduler/native handle ownership, playback time mapping and existing lookahead. Deliver source validation, correct launch frames, promotion, bounded frame queues, scheduling for trims/loops/rates and close/cancel generation safety. Gate entry parity and overload tests using held-out media; affected media/playback/app checks and GPU proofs for GPU changes. L2 trace target. No opening every decoder forever or reopening at activation after claiming prepared.

**P5 — Live transitions and recovery integration.** Entry: P2–P4 complete. Read back session launch/seek/transport and gig resilience contracts. Deliver armed destination readiness, deferred unready launches, edit invalidation, tempo rescheduling, underrun surfacing, export exact-frame waiting and diagnostic links. Gate scripted play/launch/jump/edit actions with readiness and frame assertions. L3 target. Performer gesture: launch a distant scene while another plays. No clock slowdown, silent skipped clips or in-process GPU recovery.

**P6 — Release acceptance at show scale.** Entry: all features above and meaningful instrumentation. Deliver bounded automated long-show scheduling tests plus real release transition measurements for Corrosion and a held-out large show. Include first load, cache reuse and forced eviction in the automated fixture suite; verify every scheduled boundary, not only the opening minute. A real-time duration rehearsal remains necessary evidence for sustained streaming/thermal behavior; accelerated boundary coverage does not replace it. No claim of zero hitches until observed. L3 automated target and Peter's L4 performance acceptance.

For each implementation phase run focused clippy/tests once after changes, the GPU proof gate for GPU paths, and required landing checks through `scripts/land_branch.py`. Test fixture design must bound GPU executions under AGENTS.md. No broad rendering or repeated manual crash reproduction is authorized by this draft. Draft validation itself requires references and diff checks, no app build.

## 9. Decided direction — preserve in implementation briefs

1. No arbitrary deadline that abandons required preparation.
2. Whole-show preparation and bounded residency are separate obligations.
3. Production ownership/render/decode/time mapping remain authoritative.
4. Ready requires dependency completion, valid configuration and feasible delivery.
5. GPU completion controls resource release; a timeout does not.
6. Measure the actual first-frame/transition result and keep failures auditable.

## 10. Deferred and unresolved before implementation

- Exact API signatures, cache reuse inventory, operation watchdog thresholds, memory headroom and latency margins require the phase-entry source inventory and bounded measurements; no executor should guess them.
- Peter should review the proposed unresident-jump/armed-scene behavior before P5. Instant random access to every destination is only available if that full launch working set fits.
- Automatic video transcoding or prerendering is not included. Revisit when measured media throughput or scene cost makes a show infeasible; any substitution needs an explicit quality/storage contract.
- Unbounded live-generated resources and new assets introduced mid-performance invalidate readiness. Revisit with a finite dependency declaration or explicit preparation workflow.
- Broad parallel GPU preparation is deferred until conservative admission is proven and measurements justify concurrency.
- BUG-qh04 remains a separate unresolved crash investigation. This architecture reduces uncontrolled preparation and improves coverage; it does not certify that the Corrosion fault is fixed.
