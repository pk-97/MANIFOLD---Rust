# Video IO — live texture interchange (Syphon / NDI, both directions)

<!-- index: Live video interchange with Resolume/TD/OBS — Syphon+NDI, sends as stage virtual outputs, inputs as source atoms; P1–P4 phased, supersedes CAPABILITY_ROADMAP §3 -->


**Status:** PROPOSED design, awaiting Peter approval · 2026-07-07 · Fable
**Prerequisites:** none for P1–P2 (SharedTextureBridge, stage/venue model, and the
source-atom slot all exist). P3–P4 need the NDI SDK decision (§D8, VERIFY-AT-IMPL).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

MANIFOLD joins existing rigs before it replaces them. Syphon-out makes MANIFOLD a
source inside a Resolume or TouchDesigner show on the same Mac; Syphon-in runs our
depth/segment/particle stack on someone else's live output; NDI does both across
machines. Peter, 2026-07-07, asked which tiers v1 needs: **all three** — "MANIFOLD
feeds Resolume/TD (Syphon-out) + Resolume/TD/cameras feed MANIFOLD (Syphon-in) +
cross-machine feeds (NDI, either direction)". This doc supersedes
CAPABILITY_ROADMAP.md §3 (the 2026-06-17 sketch) where they differ — notably the
send-as-graph-node idea, rejected in D2.

Companions: `MULTI_DISPLAY_DESIGN.md` (output model this extends; its §10 P6 listed
"NDI/Syphon outputs" as deferred — this doc is that item), `ML_NODES_DESIGN.md` §4
(the source-atom slot the input side fills), `GIG_RESILIENCE_DESIGN.md` (failure
doctrine), `MEDIA_BACKEND_DESIGN.md` (deferred NDI/Syphon here).

## 1. Audit — what exists (verified 2026-07-07)

Extend, don't redesign. Every piece below was verified at the cited anchor today.

| Piece | Where | State |
|---|---|---|
| IOSurface triple-buffer bridge, `Rgba16Float`, atomic `front_index` | `crates/manifold-app/src/shared_texture.rs:36` (`SharedTextureBridge`), format FourCC at `:57` | Exists — the exact object Syphon shares; Syphon-out is a conversion blit + a publish call away |
| Async GPU readback (submit / try_read / cancel, event-gated) | `crates/manifold-led/src/readback.rs:8–119` | Exists — the CPU-pixels path NDI-out reuses |
| LED output samples the compositor output texture per frame | `crates/manifold-led/src/controller.rs:78` | Exists — precedent for "an output that samples the composition" |
| Stage/venue model: `StageLayout`, venue file persistence | `crates/manifold-core/src/stage.rs:211`, `crates/manifold-io/src/venue_file.rs` | Shipped (multi-display P1) — the persistence home for sends |
| Content-thread output surface (direct present path) | `crates/manifold-app/src/content_pipeline.rs:622` (`output_surface`) | Exists — the render-side seam where textures are real |
| Source atom + generator-preset wrapping precedent | `crates/manifold-renderer/src/node_graph/primitives/gltf_texture_source.rs`; `crates/manifold-renderer/assets/generator-presets/Text.json` wraps `node.render_text` | Exists — the input side copies this shape |
| Background FFI worker handing the graph its latest result | `manifold-native` (`DepthEstimator`) | Exists — the async contract live inputs inherit |
| `node.camera` (AVCapture source atom) | `ML_NODES_DESIGN.md` §4 | **Designed, not built.** NDI/Syphon-in were already named there as "same slot later" — this doc is that later |
| Fixture source routing `Master \| layer/group` ("routing = bus") | `MULTI_DISPLAY_DESIGN.md` §7.3 | Designed, not built — D3 mirrors it. ⚠ VERIFY-AT-IMPL: if the fixture-routing enum has landed by execution time, reuse the type; command: `rg -n 'enum.*Source' crates/manifold-core/src/stage.rs` |

Classification: **genuinely new** = the Syphon/NDI FFI surfaces and the send blit.
Everything else is one wire away from existing.

## 2. Decisions

**D1 — One model, four features.** Two mechanisms (Syphon = same-Mac GPU-texture
share; NDI = network video) × two directions (out = **video sends**, in = **live
sources**). Sends share one data model regardless of mechanism; inputs share one
atom shape regardless of mechanism. No per-mechanism special cases in the model.

**D2 — Sends are virtual outputs in the stage/venue model, not graph nodes.**
A send is one more output object — like a display or an LED fixture — that samples
the composition and publishes it. Rejected: the roadmap's passthrough "send node"
dropped mid-chain (CAPABILITY_ROADMAP §3), because Peter's console-round doctrine is
"**routing = bus, never a graph node**" (MULTI_DISPLAY §7.3) and multi-display
already models every output as "a mapping: region of the render" (`MULTI_DISPLAY_
DESIGN.md:58`). Consequences, stated honestly: mid-chain taps (publish between two
effects inside one chain) are not possible; the granularity you get is the D3 source
enum. If a real show needs a mid-chain feed, the workaround is splitting the chain
across a layer + group — and if that recurs, that's the trigger to revisit (§Deferred).

**D3 — Send source = the fixture-routing selector: `Master | Layer(id) | Group(id)`.**
Same bus the LED/fixture design selects from. A send on a layer samples that layer's
post-chain output; Master samples the composite the LED controller already samples
(`controller.rs:78`). v1 sends sample at the legacy single-island composite; island-
aware region sends are deferred with their trigger (§Deferred).

**D4 — Publish happens on the content thread, render side, after composite.**
The send owns a private triple-buffered IOSurface pool (shape: `SharedTextureBridge`,
`shared_texture.rs:36`) plus one conversion blit into it, then hands the surface to
the mechanism (Syphon publish call / NDI worker channel). No new shared state: the
pool is owned by the content-side send object; cross-process safety is the
mechanism's job (Syphon and NDI both exist to do exactly that). You will want an
`Arc<Mutex>` at this boundary — no. Snapshots and owned pools, per the house model.

**D5 — Color: sends publish display-referred sRGB BGRA8 by default.** Internal is
linear `Rgba16Float` (`shared_texture.rs:57`); Resolume/TD/NDI conventions are 8-bit
display-referred. The conversion is the same math the display present path already
performs — ⚠ VERIFY-AT-IMPL: read the present-pass color conversion before writing
the blit; never synthesize it (`rg -n 'srgb|gamma|tonemap' crates/manifold-app/src/app_render.rs
crates/manifold-app/src/edr_surface.rs`). 16f/HDR pass-through publishing is
deferred (trigger: a receiving app that actually consumes it).

**D6 — Inputs are source atoms in the ML_NODES §4 slot: `node.syphon_in`,
`node.ndi_in`.** Each wrapped by a thin bundled generator preset (precedent:
`Text.json` wraps `node.render_text`) so a live feed works both as "this layer IS
the feed" and as a texture input inside any effect graph (depth/segment on a live
camera). Server/source selection is a param (directory-listed, like audio device
pickers). Async latest-frame contract inherited from the DNN workers: the graph
reads the newest complete frame; **the beat clock never waits for an input**.

**D7 — Failure behavior (the show-safe contract).** A dead or stalled input
**holds its last frame** and raises a staleness flag surfaced on the perform
surface (chrome-widget slot per GIG_RESILIENCE §7/§8) — never hard-cut to black,
never swap in a placeholder mid-show. A send that fails to publish logs, shows
status, and **never** stalls or errors the render loop — publishing is
fire-and-forget from the render's point of view. Input reappearing under the same
name silently reattaches (same doctrine as display hot-replug, MULTI_DISPLAY §11.14).

**D8 — NDI is a background worker fed by async readback.** Shape: one worker
thread per NDI send; content thread submits readback (`readback.rs` pattern),
completed CPU frames go over a bounded channel, **drop-oldest under pressure** —
the content thread never blocks on encode or network. Pixel format UYVY (+alpha
plane when needed) or BGRA per SDK guidance. ⚠ VERIFY-AT-IMPL: NDI SDK linking +
redistribution terms (proprietary SDK, free runtime; confirm current license text
allows bundling) and whether the community `ndi` Rust bindings are maintained —
`manifold-native` FFI is the fallback shape either way.

**D9 — Syphon via the official Syphon framework, never a re-derived protocol.**
Syphon's wire protocol (IOSurface IDs over mach) is private; the framework (BSD
license) is the contract. Link it via FFI/objc2 in `manifold-native` (precedent:
the existing native plugins). Same doctrine as Pro DJ Link: implement from the
published surface, never from packet/protocol guessing (synthesis-drift rule).

**D10 — Persistence: sends live in the venue profile** (`venue_file.rs`), beside
the LED patch — a send is rig-facing plumbing ("my Resolume expects a feed named
X"), not show content. Round-trip rule: a venue loading with a send whose
mechanism is unavailable (no NDI SDK, macOS < required) keeps the send
inert-but-present and warns loudly; silent dropping is the forbidden move of load
paths. Published names: `MANIFOLD — <send name>`.

## 3. Data model

Owner: content thread (mutations via `EditingService` commands like every model
write). Serialization: venue file (V2 zip infra). Thread crossing: UI sees sends
via the existing snapshot path; live status (connected clients, staleness) rides
`ContentState` like LED status does.

```rust
// manifold-core (stage.rs neighborhood)
pub struct VideoSendId(pub u64);

pub enum VideoSendMechanism { Syphon, Ndi }

pub enum VideoTapSource {          // ⚠ VERIFY-AT-IMPL: unify with fixture routing enum if landed
    Master,
    Layer(LayerId),
    Group(LayerId),
}

pub struct VideoSendDef {
    pub id: VideoSendId,
    pub name: String,              // publishes as "MANIFOLD — {name}"
    pub mechanism: VideoSendMechanism,
    pub source: VideoTapSource,
    pub enabled: bool,
}
```

Input side adds **no model types** — `node.syphon_in` / `node.ndi_in` are ordinary
primitives with params (source name, latest-frame behavior), state held per-runtime
like every DNN atom. Serialization is the graph JSON it already has.

Runtime (not serialized, content-side): per-send publisher object owning the
IOSurface pool + blit pipeline (Syphon) or the readback + worker channel (NDI);
per-input receiver worker owning discovery + latest-frame slot.

## 4. The plausible-wrong turns, forbidden by name

1. **Send as a graph node** — rejected in D2. You will reinvent it because the
   roadmap doc still sketches it; don't.
2. **`Arc<Mutex>` at the publish boundary** — no; owned pools + channels (D4/D8).
3. **Blocking the content thread on readback/encode/network** — the LED path never
   does; neither does this (D8). Any `wait`/`recv` on the content thread in this
   feature is a bug.
4. **Publishing from the UI thread** because the UI "has" the shared texture — the
   publish happens where the texture is rendered (D4), and the UI-side surface is
   the *display* copy, not the send.
5. **Re-deriving the Syphon mach protocol or NDI wire format** (D9) — link the
   official surfaces.
6. **A special "live input layer" type** — inputs are source atoms + generator
   presets (D6), nothing new at the layer model.
7. **Silent fallback when a source dies** (black frames, auto-swap) — D7 is the
   contract: hold-last-frame + loud staleness.

## 5. Phasing

Test scope per phase: focused `-p` tests during the phase; one workspace sweep +
clippy at each phase's end (these phases don't touch GPU-tested primitives' shaders,
so no `gpu-proofs` run unless a blit kernel lands in manifold-renderer — then run
the focused gpu test for it). UI-flow (L3) doesn't cross process boundaries, so
demos gate at L2 with an external receiver + screenshot, plus in-process loopback
tests as the mechanical gate.

### P1 — Syphon-out vertical slice (one session)

- **Entry:** anchors in §1 re-verified (run the audit table's commands).
- **Read-back:** this doc §2 D2/D4/D5/D9, GIT discipline, forbidden moves above.
- **Deliverables:** `VideoSendDef` + commands (add/remove/enable, venue-persisted);
  Syphon framework FFI in `manifold-native`; content-side publisher (pool + sRGB
  blit); one default send "Program" (Master); minimal sends list UI in the
  settings/outputs surface (App-shell panel taxonomy — smallest honest UI, not a
  placeholder).
- **Gate (positive):** in-process loopback test — publish a generated test pattern,
  receive it back via a Syphon client handle, PNG-compare within blit tolerance.
  Venue round-trip test: save venue → reload → send re-publishes without a command.
  (negative): `rg -n 'Arc<Mutex' crates/manifold-app/src crates/manifold-core/src --glob '!*test*'`
  shows no new hits vs baseline; `rg -n 'node\.(syphon|video)_send'` returns zero
  (proves D2 — no send node exists).
- **Acceptance demo (L2):** Resolume or Syphon Simple Client on the same Mac shows
  MANIFOLD's program output live; screenshot committed with the landing report.
- **Performer gesture:** mid-set, toggle the send off and on from the UI — Resolume
  side blanks and returns; MANIFOLD's own output never hiccups (trace run per the
  content-thread work gate: no frame >20ms attributable to the send).

### P2 — Syphon-in source atom + LiveInput generator (one session)

- **Entry:** P1 landed (its send is this phase's test fixture).
- **Deliverables:** `node.syphon_in` (server-directory param, latest-frame slot,
  staleness output/flag), bundled `LiveInput.json` generator preset wrapping it,
  perform-surface staleness indicator (chrome-widget slot).
- **Gate:** loopback parity — P1 publishes a known pattern, a graph with
  `node.syphon_in` receives it, headless render PNG-compares. Kill the sender
  mid-test: output holds last frame and the staleness flag flips within one second
  (that's D7, tested, not promised). Held-out input: receive from a non-MANIFOLD
  sender (any Syphon demo app) at least once — L2, screenshot.
- **Performer gesture:** drop the LiveInput generator on a layer, pick a server
  from the param dropdown, feed appears; unplug the source — the layer holds, the
  indicator lights, the show does not go black.

### P3 — NDI-out (one session)

- **Entry:** P1 landed; NDI SDK decision resolved (D8 verify) — **blocking, decider
  Peter** if the license read is ambiguous.
- **Deliverables:** NDI FFI, per-send worker (readback → bounded channel →
  UYVY/BGRA encode → NDI send), `VideoSendMechanism::Ndi` live, drop-oldest
  pressure behavior instrumented (drops counted, visible in status).
- **Gate:** NDI receive loopback via the SDK's receiver API where possible; held-out:
  OBS (or NDI Video Monitor) on another machine renders the feed — L2 screenshot.
  Trace run: content-thread cost of an active NDI send stays in budget (no frame
  >20ms); the readback is async by construction, prove it with the trace, not prose.
- **Performer gesture:** kill the wifi/cable mid-send — MANIFOLD's render never
  stutters; status shows the send down; reconnect resumes without a restart.

### P4 — NDI-in (one session)

- **Entry:** P2 + P3 landed (P2 defines the atom contract, P3 brings the SDK).
- **Deliverables:** `node.ndi_in` in the same slot with discovery, same staleness
  contract, same generator preset (mechanism param or sibling preset — executor
  choice, behavior identical).
- **Gate:** cross-machine loopback (P3 send → P4 receive on loopback interface in
  CI if the SDK allows; otherwise fixture capture + held-out live receive at L2).
  Workspace sweep + clippy here (final phase).
- **Performer gesture:** FOH laptop sends camera over NDI; MANIFOLD runs person-
  segmentation on it inside a generator graph, live.

## 6. Decided — do not reopen

1. Sends are stage/venue virtual outputs; no send graph node (D2).
2. Send source = `Master | Layer | Group` bus selector; no mid-chain taps (D2/D3).
3. Publish on content thread, render side; owned pools, no new shared state (D4).
4. Default publish format: sRGB BGRA8 display-referred (D5).
5. Inputs = source atoms + generator wrappers in the ML_NODES slot; no layer type (D6).
6. Dead input → hold-last-frame + loud staleness; dead send → never touches render (D7).
7. NDI = background worker, drop-oldest, never blocks content (D8).
8. Official Syphon framework / NDI SDK; no protocol re-derivation (D9).
9. Sends persist in the venue profile; unresolvable sends stay inert + warned (D10).
10. Beat clock never waits for a live input (D6).

## 7. Deferred (with revival triggers)

- **Mid-chain tap node** — revive only if the layer+group split workaround recurs
  in real shows (D2).
- **Island-aware / region sends** — trigger: multi-display P2 (islands) landing;
  until then sends sample the legacy composite (D3).
- **HDR / 16f publishing** — trigger: a receiving app that consumes it (D5).
- **Spout (Windows)** — arrives with the Vulkan/Windows backend, same role as
  Syphon, not separate work (roadmap §3 stands).
- **ScreenCaptureKit input** — same source slot, per ML_NODES §14; unchanged.
- **Recording taps** (send → disk) — MEDIA_BACKEND owns export; a send is not a
  recorder.
