# UI Automation — the agent drives the instrument

**Status:** IN PROGRESS — P1/P2 shipped. P3 primary-window implementation and initial P4 generator flow verified on branch `codex/live-ui-control`. Handover hardening and disconnect regression added 2026-09-06; native-user interruption acceptance and landing remain (BUG-m7nb; gate blocked by existing Downloads fixture access).
**Prerequisites:** none. P1–P2 extend the shipped ui-snap harness; P3–P4 are self-contained dev infra.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase. P3–P4 carry pre-flight re-derivation commands.

Peter, 2026-07-03: *"we will likely need custom infra so you can interact with Manifold and test UI and UX features in depth"* — and, on scope: the agent should *"interact with the app as a first class feature… widgets, gizmos, etc"*, because *"this will be a huge help in verifying features and systems where unit tests can't. Automated integration testing!"*

The governing insight: **Playwright works because the DOM gives it three things — find, act, wait. MANIFOLD's bitmap UI already has two-thirds of the substrate**: a real retained tree with durable widget identity (`WidgetId`), a headless harness that renders the real UI and drives one real click, and a proven input seam. This design finishes the triad: the tree dump becomes the selector surface (the "DOM"), a gesture driver acts by widget identity through the production input path, and explicit sync replaces auto-wait. One interaction core, two transports: the headless harness (scripted, deterministic) and a dev-only live door into the running app.

Companion docs: `docs/HEADLESS_UI_HARNESS.md` (the shipped harness this extends — read it whole before P1) · `docs/MCP_INTERFACE_DESIGN.md` (the product AI surface; section 9 pins how it may later forward to this layer — this design is NOT part of it) · `docs/archive/INPUT_IDENTITY_UNIFICATION.md` (why `WidgetId` exists and how input tracks it).

## Live primary-window contract — 2026-09-06

Peter: “agents need to use the same app people use”; reduce repeated screenshots
with compact observations and reliable commands. This section governs live work;
the historical TCP/thread/`EventLoopProxy` P3 proposal is superseded.

Audit at base `909ad80bc`: `window_input.rs::input_cursor_moved`,
`input_mouse_input`, `input_mouse_wheel`, and `input_keyboard` already own native
window input. `about_to_wait` renders on the existing frame cadence.
`manifold_ui::automation` owns selectors, stable widget targets and drag math;
`viewport.visible_clip_rects` supplies actual clip geometry. The headless runner
is fixture-backed and is not a live session. Extend these owners.

- **L-D1:** feature `ui-automation` plus explicit `MANIFOLD_UI_SOCKET` enables a
  per-instance Unix socket. Existing private directory, owner-only socket, no TCP,
  shell evaluation, arbitrary file read, or project mutation endpoint. Shipping
  builds without the feature have no listener. No new dependency/thread/lock.
- **L-D2:** `Application` owns `Option<LiveUi>`, serviced once after a real render
  tick. `LiveUi` owns transport and at most one pending gesture. No UI references
  cross threads; content ownership and undo remain unchanged.
- **L-D3:** reuse `AutomationAction`/`AutomationTarget`. Own the deserialized
  `Surface.surface` string instead of leaking it; JSON shape stays compatible.
  Live Pointer/Key/Text/Step reach the exact native input handlers. Unsupported
  headless verbs fail explicitly, particularly direct `SetParam` fixture setup.
- **L-D4:** one event per real frame permits drag thresholds and state sync to
  run. Resolve against current layout; reject absent/ambiguous/offscreen targets.
  Targets are checked again immediately before the first press or wheel event;
  moved/hidden targets fail instead of retargeting. Resize/scale changes interrupt.
  Reply means input dispatched, not a successful project edit. Flow expectations
  inspect the real content snapshot with a bounded deadline and never retry edits.
- **L-D5:** reuse WidgetId, names, text, ranges through existing parameter rows,
  and clip geometry. No second widget registry or mirrored mutable project.
  Observe is on-demand; idle service does no tree walk, serialization or allocation.
- **L-D6:** primary window first. Native menus, file dialogs and other windows
  remain computer-use operations. Screenshots verify appearance when relevant.

Rejected alternatives: full AccessKit coverage first would add broad focus and
screen-reader design before proving the requested workflow. Driving fixture
handlers in a headless harness would not meet live fidelity. The original TCP
worker/proxy design adds a thread/channel and network exposure unnecessarily:
the live event loop already ticks. Bounded nonblocking socket work is the smaller
seam. Its cost is frame-paced command latency; this is a debugging tool, not a
real-time performance-control protocol. The kill test is live click/drag/undo,
not a compile or a harness screenshot.

Committed seams: `LiveUi::bind(&Path) -> io::Result<LiveUi>`;
`Application::tick_live_ui(&mut self)`; transport `bind`, `poll() -> Option<Value>`,
`reply(Value)`, `connected() -> bool`, `generation() -> u64`. Transport is UI-thread
resident, nonblocking, one request/client, 64 KiB requests, 2 MiB replies,
64 KiB IO per direction per poll, five-second client timeout. Generation fences
prevent delivery of old replies to new clients; disconnect releases held input.

Wire v1: newline JSON with optional `id`; `op` is `observe` (optional `contains`),
`resolve` (`target`), `act` (`action`), or `timeline_point` (`beat`, `layer`).
Coordinates are primary-window logical pixels. Observe returns protocol, widget
metadata, clip geometry and a content snapshot summary. Its `input` diagnostic
reports mouse/selection state, logical cursor and the last interruption (reason,
request id, frame, held-button flag, remaining events and cursor before cleanup). Responses carry `ok`,
`frame`, and `data` or `error`; action completion carries `dispatched` and `state`.
The Python standard-library client `scripts/live_ui.py` supplies requests,
common gestures and sequential JSON flows with read-only expectation polling.

Invariants/checks: transport tests prove permission, framing, timeout, bounded
write and reply isolation; request tests reject unknown operations/fields;
existing selector tests prove exact-one matching. Source check: live module may
not name `ContentCommand`, `EditingService`, or headless fixture dispatch.
Native and automated modifiers share `input_modifiers` as well.

Execution: entry base as above; read back shared input and selector contracts.
Deliver live module/transport, feature wiring, client and reproducible generator
flow. Gate with focused app/UI clippy/tests, feature-enabled transport tests,
Python client tests, then the running-window workflow and undo/redo. Observe
32 bars as 128 beats in 4/4 and playback advancement; produce a final native
screenshot. Use the existing landing gate, keeping other worktrees untouched.

Deferred: editor/output windows (trigger: first multiwindow workflow), full
accessibility (trigger: dedicated accessibility scope), native-dialog automation
(use OS computer tools), pixel capture in protocol (OS screenshot already works),
arbitrary scripts/plugins inside the app (not required). New behaviours extend
the shared input/target vocabulary and add a live flow; no direct model setters.

---

## 1. Audit — what exists (verified 2026-07-03)

Extend, don't redesign. Every piece below is shipped and load-bearing.

| Piece | Where | State |
|---|---|---|
| Durable widget identity | `crates/manifold-ui/src/node.rs:317` (`WidgetId`) | SHIPPED. Parent-id ⊕ sibling-salt through splitmix64 (`node.rs:334`). Stable across full rebuilds; explicit keys (`tree.rs:157` `add_node_keyed`) survive sibling reordering. Tests: `tree.rs:1242` (`widget_id_is_stable_across_clear_and_rebuild`), `tree.rs:1290` (`explicit_key_survives_sibling_reordering`). |
| Interactive-node reverse lookup | `crates/manifold-ui/src/tree.rs:48` (`widget_to_node`), `tree.rs:820` (`node_for_widget`) | SHIPPED. Interactive nodes only; debug-asserts on collision (`tree.rs:252-259`). |
| Input system tracks by WidgetId | `crates/manifold-ui/src/input.rs:473` (`process_pointer`), `input.rs:637` (`process_key`), `input.rs:439` (`drain_events`) | SHIPPED. Resolves WidgetId → live NodeId only at event emission. |
| Tree hit-testing | `crates/manifold-ui/src/tree.rs:577` (`hit_test`) | SHIPPED. Topmost interactive node at point; respects disabled + clip ancestors. |
| Headless harness (render + dump + one interaction) | `crates/manifold-app/src/ui_snapshot/` (feature `ui-snapshot`), entry `mod.rs:35` | SHIPPED (`docs/HEADLESS_UI_HARNESS.md`). Scenes: timeline/states/inspector/graph/editor/all. Real `UIRoot` + `state_sync` path. |
| Tree dump | `ui_snapshot/dump.rs:12` (`dump_tree`) | SHIPPED. Emits per node: NodeId index/gen, parent, type, rect, style, text, flags, draw order. **Does NOT emit WidgetId or a component name** — the P1 gap. |
| Interaction driver (seed) | `ui_snapshot/interact.rs:18` (`apply`) | SHIPPED, two verbs (`select:<layer>`, `open:settings`). Proves the seam: resolve rect from built tree → `UIRoot::pointer_event` Down+Up → `drain_events` → real `Panel::handle_event` dispatch. **Has a silent fallback on miss (`interact.rs:62-67`) — removed in P2 (section 6 seam brief).** |
| UIRoot injection points | `crates/manifold-app/src/ui_root.rs:989` (`pointer_event`), `ui_root.rs:1011` (`key_event`) | SHIPPED. Take logical position / key + a caller-supplied `time: f32` — the clock is already a parameter, which is what makes deterministic scripting possible. |
| Live input dispatchers (one owner, both windows) | `crates/manifold-app/src/window_input.rs:103` (`input_cursor_moved`), `:118` (`input_mouse_input`), `:134` (`input_mouse_wheel`), `:1517` (`input_keyboard`) | SHIPPED. The single entry per winit event; window routing + scroll normalization + cursor projection live here. |
| Custom hit-test surfaces | `crates/manifold-ui/src/graph_canvas/hit.rs:60` (`hit_test`) — nodes/ports/wires; timeline clips via `crates/manifold-ui/src/clip_hit_tester.rs` | SHIPPED. These targets are invisible to `UITree::hit_test` and to the dump — the P1 registration gap (section 5). |
| Live sync primitives | `crates/manifold-app/src/content_state.rs:62` (`ContentState.data_version`), `crates/manifold-ui/src/tree.rs:56` (`structure_version`) | SHIPPED. The wait-condition substrate for the live door (section 7). |
| Event loop | `crates/manifold-app/src/main.rs:112` (`EventLoop::new()` — no user-event type), `app.rs:1628` (`ApplicationHandler`), `app.rs:2533` (`about_to_wait`) | SHIPPED. No proxy/wakeup plumbing exists yet; P3 adds it. |
| Request/reply channel shape | `docs/MCP_INTERFACE_DESIGN.md` section 3 (`McpRequest { kind, reply }`) | DESIGNED, not built. section 4 (Security model) reuses the *shape* (per-request bounded(1) reply channel), not the crate. |

Re-derivation (run at any phase start; if counts differ from above, stop and re-inventory):
`rg -n "fn hit_test" crates/manifold-ui/src/` · `rg -n "pointer_event|key_event" crates/manifold-app/src/ui_root.rs` · `rg -n "pub\(crate\) fn input_" crates/manifold-app/src/window_input.rs`

**Baseline-review addendum (2026-07-05, anchors spot-reverified):** all audited symbols
still exist; line numbers have drifted (`pointer_event` 989→1106, `key_event` 1011→1128,
`UITree::hit_test` 577→613, `widget_to_node` 48→53, `node_for_widget` 820→858, the
`input_*` dispatchers +13 each) — trust the re-derivation commands, not the baked numbers.
Two substantive changes since the audit: (1) **a new custom hit-test surface shipped
2026-07-04** — automation lanes (`crates/manifold-ui/src/automation_hit_tester.rs`,
`hit_test_automation` / `AutomationLaneScreen`) — added to the D5/P1 scope in section 5;
(2) **`interact.rs` grew ~10×** (automation-lanes + preset-picker verification work);
the section 6 seam brief's baked inventory is stale — its re-derivation command is now mandatory
before P2 touches the file (the miss-fallback currently sits near `interact.rs:608`).

## 2. Decisions

- **D1 — The tree dump is the selector surface.** The extended dump (WidgetId + component name + custom-surface targets, section 3/section 5) is the one machine-readable description of "what is on screen"; the agent navigates it like a DOM and every selector resolves against it. Rejected: an AccessKit-style separate semantic tree — a second structure to keep in sync with the real one, when the real one is already walkable and already carries text, type, hierarchy, and state flags.
- **D2 — Act by identity, resolved to coordinates at act time.** A script targets a widget (by name/text/structure query, section 3); the driver resolves its rect from the *current* build and synthesizes input at that point. Rejected by name: **coordinate scripting** ("click at (412, 87)") — it rots on every layout change and is the tempting shortcut every executor will reach for. A raw `point:` target exists (section 4) for empty-canvas cases only; a script that uses `point:` where a widget target exists fails review.
- **D3 — One transport-agnostic core, two transports.** A single `AutomationRequest` enum (section 4) serviced on the UI thread. Transport A: the ui-snap script driver (headless, P2). Transport B: a dev-only localhost server (live, P3). Rejected: building this into `manifold-mcp` v1 — that couples a dev instrument to wave-3 product work and its tokio runtime; the MCP server may later grow a *gated* `ui` tool group that forwards to this same enum (section 9 forward constraint), which is why the enum, not the transport, is the contract.
- **D4 — Injection enters at the proven seams, one per mode.** Headless: `UIRoot::pointer_event`/`key_event` (`ui_root.rs:989/1011`) + real panel `handle_event` dispatch — exactly the seam `interact.rs` proved. Live: the `window_input.rs` dispatchers (`input_*`), so window routing, scroll normalization, and cursor projection all run. Rejected by name: **OS-level event synthesis (CGEvent/AppKit)** — needs a window-server session, can't run headless, races the real cursor, and tests the OS instead of MANIFOLD.
- **D5 — The hit-test ⇒ register rule.** Any surface that answers its own hit-testing (graph canvas, timeline clips, future 3D gizmos) implements `HitTargets` (section 5) and appears in the dump. A new interactive surface that doesn't register is incomplete by definition — this is what makes the agent first-class rather than "can click some things". Peter's scope quote above is the mandate.
- **D6 — No silent fallbacks, ever.** A target that doesn't resolve, or a synthesized gesture that misses, fails the script loudly with the dump attached as evidence. The existing `interact.rs` miss-fallback (`interact.rs:62-67`) is deleted in P2. (House rule: `feedback_no_silent_fallbacks_or_interim_stopgaps`.)
- **D7 — The script owns the clock.** Headless runs pass explicit time into `pointer_event`/build; a `step` action advances frames by fixed dt. No wall-clock reads in the driver. Same run → same pixels → same dump, every time.
- **D8 — Names are `&'static str` component names; dynamic identity comes from structure.** Panels name interaction points with static strings (`"layer_header.mute"`); *which row* comes from the selector's ancestor query (section 3), not from allocating per-row name strings. The editor rebuilds its tree every frame — per-node `String` names would be a per-frame alloc on the UI thread. Hot-path rule wins; `Vec<Option<&'static str>>` costs nothing.
- **D9 — Live control is explicitly enabled.** Feature `ui-automation` plus `MANIFOLD_UI_SOCKET`, owner-private Unix socket, no new thread/channel. The current live contract above supersedes the original TCP proposal.
- **D10 — Minimal assertions in the script driver; pixel goldens stay deferred.** `assert` steps cover exists / text-equals / count / rect-within (section 6). Everything richer is the reading agent's job over the emitted dumps. Golden-image diffing remains deferred exactly as `HEADLESS_UI_HARNESS.md` decided — a moving visual design would make it noise.

## 3. Selector model — the dump becomes the DOM

`dump_tree` (`ui_snapshot/dump.rs:12`) gains three fields per node, all additive:

- `widget`: the `WidgetId` raw value as hex (`node.rs:347` `raw()`), emitted for interactive nodes. The durable handle a script acts on.
- `name`: the static component name (D8), when registered. Registration API: a `name: Option<&'static str>` parameter on the keyed/interactive node builders in `tree.rs` (exact plumbing free to the executor; storage is `Vec<Option<&'static str>>` alongside `widget_ids`, `tree.rs:34`).
- `targets`: for nodes owning a custom surface — the `HitTargets` enumeration (section 5).

**Selector = a structural query over the dump**, resolved by the driver:

```json
{ "name": "layer_header.mute", "under_text": "PLASMA" }
{ "text": "Bloom", "type": "Button", "nth": 1 }
{ "target": { "surface": "graph_canvas", "kind": "port", "label": "Source" } }
```

Resolution: filter nodes by `name`/`text`/`type`; `under_text` walks ancestors until a node whose `text` matches (how "the mute button of the PLASMA row" works without per-row name allocation); `nth` disambiguates; exactly-one match required — zero or >1 is a hard failure listing the candidates (D6). Custom-surface targets resolve through the owning node's `targets` list.

**Naming pass scope (P1):** register names at high-value interaction points only — layer header controls, transport, inspector card controls, graph-editor chrome. Coverage grows organically; the selector language works unnamed via text/type/structure, so an unnamed widget is reachable, just less convenient. Do not attempt an exhaustive naming sweep.

## 4. Action model — the core enum

Lives in `manifold-ui` (no app dependencies; both transports and the harness reach it). Committed shape:

```rust
/// One automation request. Transport-agnostic: the ui-snap script driver
/// (headless) and the opt-in live connection both compile scripts down to this.
pub enum AutomationAction {
    /// Resolve `target` against the current build, synthesize the gesture
    /// through the production input path (D4).
    Pointer { target: AutomationTarget, gesture: Gesture },
    Key { key: Key, modifiers: Modifiers },
    /// Text through the real TextInput path (focused field).
    Text { text: String },
    /// Advance the deterministic clock by `frames` at fixed `dt` (headless);
    /// in live mode, wait `frames` real frames.
    Step { frames: u32 },
    /// Emit the extended dump (section 3) to the run's output dir / reply.
    Dump,
    /// Emit a PNG of the current UI to the run's output dir / reply.
    Snapshot,
    /// D10 assertion; failure = loud stop with dump attached.
    Assert { selector: AutomationTarget, check: AssertCheck },
}

pub enum AutomationTarget {
    Query(SelectorQuery),          // section 3 structural query
    Widget(u64),                   // a WidgetId raw value from a prior dump
    Surface { surface: &'static str, kind: String, label: String }, // section 5
    Point(Vec2),                   // escape hatch — D2 restrictions apply
}

pub enum Gesture {
    Click { modifiers: Modifiers },
    DoubleClick,
    Hover,
    /// Down at target, interpolated Move steps (real drag thresholds must
    /// fire), Up at `to`. `steps` ≥ 2.
    Drag { to: AutomationTarget, steps: u32 },
    Scroll { delta: Vec2 },
}

pub enum AssertCheck { Exists, TextEquals(String), Count(u32), RectWithin(Rect) }
```

`Key`/`Modifiers` are the existing `input.rs` types. Window addressing: each request set runs against one `WindowTarget` (`Primary` / `Editor`) — the workspace split is real (`window_input.rs:12-14`); ⚠ VERIFY-AT-IMPL: exact workspace access for the editor's `UIRoot` — read `crates/manifold-app/src/window_registry.rs` and `ui_root.rs` before P2 wiring.

Drag matters most: it is the gesture the current harness cannot do, and it is where the instrument lives (clips, sliders, wires, node positions). Interpolated `Move` events must pass through the same threshold logic real drags hit — a Down/Up teleport is forbidden (it would "pass" flows a user cannot perform).

## 5. Custom surfaces — the hit-test ⇒ register rule

`UITree::hit_test` cannot see inside the graph canvas or the timeline lane body; those surfaces run their own hit-testing (`graph_canvas/hit.rs:60`, `clip_hit_tester.rs`). The rule (D5): **whatever a surface can hit-test, it must enumerate.**

```rust
/// Implemented by every surface that answers its own hit-testing.
/// The enumeration is the automation-visible mirror of hit_test:
/// every kind of thing hit_test can return appears here with its
/// current rect and a stable label.
pub trait HitTargets {
    fn surface_id(&self) -> &'static str;                 // "graph_canvas"
    fn enumerate(&self, out: &mut Vec<HitTargetEntry>);
}

pub struct HitTargetEntry {
    pub kind: &'static str,   // "node" | "port" | "wire" | "clip" | …
    pub label: String,        // node title, port name, clip name — what a human would say
    pub rect: Rect,           // current screen rect (post camera/scroll transform)
    pub payload: String,      // stable domain id (graph doc id, clip id) for exactness
}
```

- P1 implements it for the **graph canvas** (nodes, ports, wires — the model + camera transform in `graph_canvas/model.rs` / `camera.rs` already hold everything `enumerate` needs), the **timeline lanes** (clips), and the **automation lanes** (`automation_hit_tester.rs` — strips and breakpoints; shipped 2026-07-04, after the original audit; driving this surface is how verification-debt entry VD-001 gets burned down, so it is in scope, not deferred).
- Smaller self-hit-testing surfaces found in the 2026-07-05 re-sweep — timeline marker flags (`panels/viewport/interaction.rs` `hit_test_marker_flag`) and dock edges (`dock.rs`) — are D5-eligible but **deferred**: implement `HitTargets` for each the first time a flow script needs it (the trigger), not speculatively in P1.
- `payload` carries the domain-stable id — for graph nodes that is the `(scope_path, u32 doc id)` addressing already pinned by `project_graph_command_node_addressing`; for clips, the clip id. Labels are for humans and agents; payloads are for exactness.
- Future surfaces inherit the rule by construction: REALTIME_3D's viewport gizmos (`docs/REALTIME_3D_DESIGN.md`) implement `HitTargets` when built — translate-X handle as `kind: "gizmo"`, `label: "translate-x"`. No hard edge; noted here so neither design is surprised.
- Enumeration is on-demand (dump time only), never per-frame. Zero hot-path cost.

## 6. Headless script driver (extends ui-snap)

`cargo xtask ui-snap <scene> --script <file.json>` — a JSON array of section 4 actions, executed in order against the scene fixture. Artifacts land in `target/ui-snapshots/<scene>/run-<script-stem>/`: numbered PNGs and dumps at each `Snapshot`/`Dump` step, plus `result.json` (per-step outcome, resolved targets, assert results). Exit 0 only if every step succeeded (D6, D10).

The `select:`/`open:` `--interact` verbs become sugar for one-step scripts; `interact.rs`'s dispatch rewires to the section 4 core.

**Seam brief — `interact.rs` miss-fallback removal (P2):**
- Old: on synthesized-click miss, warn and fall back to direct id match (`interact.rs:61-67` — the WARNING eprintln plus `clicked.unwrap_or(idx)`).
- New: a miss returns a step failure carrying the dump; no fallback path exists. Delete the fallback arm, not just the warning.
- Call-site inventory: `interact::apply` has exactly one caller (`ui_snapshot/mod.rs:116`). Re-derive: `rg -n "interact::apply" crates/manifold-app/src/` — if >1, stop and list.
- Deletion gate: `rg -n "fell back|unwrap_or\(idx\)" crates/manifold-app/src/ui_snapshot/` → zero hits.

Determinism (D7): the driver owns a monotonically stepped clock; `Step` advances it by `frames × dt` at the fixture's fixed dt. ⚠ VERIFY-AT-IMPL: where time currently enters `UIRoot` build/animation (the `time: f32` on `pointer_event` is caller-supplied; confirm no other wall-clock reads on the headless path — `rg -n "Instant::now|SystemTime" crates/manifold-app/src/ui_snapshot/ crates/manifold-ui/src/`).

## 7. Using the live connection

The current live contract above replaces the original P3 TCP worker proposal.
The ordinary application singleton is preserved: close the existing MANIFOLD
instance before launching a test build. The launcher refuses a failed startup;
it never kills another instance or bypasses the lock.

From the worktree, build with the shared build lock:

```sh
.claude/scripts/with-build-lock.sh cargo build -p manifold-app --features ui-automation --manifest-path "$PWD/Cargo.toml"
python3 scripts/launch_live_ui.py
```

The launcher prints the exact app bundle, PID, log and socket paths. Use that
socket with the CLI; `<socket>` below is the returned path:

```sh
python3 scripts/live_ui.py --socket '<socket>' observe --contains speed
python3 scripts/live_ui.py --socket '<socket>' click --name transport.play
python3 scripts/live_ui.py --socket '<socket>' key Z --command
python3 scripts/live_ui_generator_demo.py --socket '<socket>'
```

The demo refuses an existing project: it requires one empty Layer 1, default
120 px/beat zoom, and 4/4. It creates a Caustics clip at beat zero lasting 128
beats, configures speed/scale/shine through the value editors, proves clip and
parameter undo/redo, and checks playback advances then stops. It does not save
or publish a project. Native dialogs still use ordinary computer-use tools.

`live_ui.py request '<JSON>'` exposes the protocol; `run <flow.json>` executes
an array of `{"request": {...}}` and read-only `{"expect": {"data.path": value}}`
steps. Expectations poll with a bounded timeout. Edits are never retried.
Native mouse/keyboard input interrupts a pending sequence, which releases held
input and reports an error; inspect state before continuing. Automated pointer
sequences restore the pre-gesture cursor and modifiers on completion or failure,
so a stationary native mouse click uses the user's position. New actions refuse
an already-held native pointer. Interruption releases input; timeline move/trim
uses the existing Escape rollback. Other controls retain their normal Escape
and mouse-release semantics; interruption is not a general transaction rollback.

After the generator demo, `python3 scripts/live_ui_safety_demo.py --socket '<socket>'`
checks disconnect during a 60-step trim, rollback, released input, cursor
restoration and subsequent trim/undo/redo. It requires the prepared demo state
and a recorded interruption while the button was held; no timing-only success.
Native computer-use testing requires an idle Mac because OS focus/input is
shared with the user. Close the test window after each live testing session.

Geometry is resolved from the current tree. Raw Point targets remain explicit
coordinates, and drag destinations are fixed for the gesture; they have no
identity/staleness guarantee. Use named/widget targets where available. Widget
targets check visibility, ancestor clipping and occlusion; clip surface targets
currently check window bounds only. Postconditions remain necessary.

## 8. What does NOT change

- `EditingService` stays the sole mutation gateway; automation mutates nothing directly — it produces input events.
- The two-thread model is untouched. The live door is a UI-thread *requester*; it never touches the content thread (its only content-thread contact is reading `ContentState` snapshots the UI already has).
- `UIInputSystem`, panel `handle_event` dispatch, and the winit dispatchers keep their exact behavior — automation enters through them, never around them.
- Shipping builds are byte-identical in behavior: both features (`ui-snapshot`, `ui-automation`) are compiled out.

## 9. Phasing

Forbidden across all phases: coordinate scripting where a widget target exists (D2) · any fallback on miss (D6) · wall-clock in the headless driver (D7) · per-frame allocation for names or target enumeration (D8, section 5) · a parallel "test-only" input path that bypasses `process_pointer`/panel dispatch (the whole point is exercising the real one).

- **P1 — Selector surface. ✅ SHIPPED 2026-07-05** (L2 — editor/timeline/automation dumps read at landing: 107 graph targets with scope/node/port payloads, 9 clips with clip-id payloads, 7 automation strips/breakpoints, named transport + layer-header widgets; `cargo test -p manifold-ui --lib` 595/595; clippy clean). Landing note: the `custom_surfaces` enumeration is a sibling top-level dump key, not the per-node `targets` field the section 3 prose implies — no `UITree` node owns the graph canvas / clip / automation surfaces (they're addressed by screen rects), so the enumeration is carried alongside `nodes`; still strictly additive. Minor gap → VD-005. `manifold-ui`: name storage + builder plumbing (D8), `HitTargets` trait + graph-canvas, timeline-clip, and automation-lane impls (section 5); `manifold-app`: dump gains `widget`/`name`/`targets` (section 3); naming pass at the section 3 scope. Read-back: section 3, section 5, `dump.rs` whole, `graph_canvas/hit.rs` + `model.rs`, `automation_hit_tester.rs` whole. Deliverables: extended dump visible in `ui-snap editor --dump` and `timeline --dump`. Gate (positive): editor-scene dump lists every node/port the canvas `hit_test` can return, with payload ids; timeline dump lists every fixture clip and every automation-lane strip/breakpoint visible in the fixture; `cargo test -p manifold-ui --lib` green including new tests for name storage + a `HitTargets` enumeration test per impl. Gate (negative): `rg -n "String" crates/manifold-ui/src/` shows no per-node name `String` storage in `tree.rs` (names are `&'static str`). **Acceptance demo (L2, section 10):** the two dumps above are the artifacts — the landing reviewer reads them and confirms named widgets, graph targets with payload ids, clips, and automation-lane targets are present; absence of any category is a gate failure, not a note. Test scope: `-p manifold-ui --lib` + the two ui-snap runs; no workspace sweep (additive dev surface, no product path touched).
- **P2 — Script driver. ✅ SHIPPED 2026-07-05** (L2 — both proving flows exit 0; drag-clip moved Plasma 1's clip 230→314px through the real `process_pointer`→`process_events`→`InteractionOverlay`→`AppEditingHost` path with 6 interpolated steps, before/after PNGs read at landing; `cargo test -p manifold-ui --lib` 604/604; clippy clean; D6 hard-failures verified — zero-match and ambiguous Pointer both exit non-zero with candidates; both negative gates zero hits). Landing notes: the enum lives in `manifold-ui`, which gained a `serde` dependency (workspace, for the JSON `--script` format the doc mandates — `AutomationTarget` uses a manual `Deserialize` that leaks the `Surface.surface` string to keep the doc's committed `&'static str` type); `AutomationAction::Text` has no headless injection seam and fails loudly (neither proving flow needs it); the headless drag routes clip mutations through a driver-held `crossbeam` channel whose receiver is never drained (`ContentCommand::send` only errors on disconnect), so the real mutation lands on the scene `Project` with no live content thread. `AutomationAction` enum in `manifold-ui` (section 4 committed shape), gesture synthesis incl. interpolated drag, selector resolver, `--script` runner + artifacts + `result.json`, `--interact` rewired as sugar, `interact.rs` fallback deleted (section 6 seam brief). Read-back: section 4, section 6, `interact.rs` whole, `ui_root.rs:989-1030`. Deliverables: two proving scripts committed under `scripts/ui-flows/`: `select-and-inspect.json` (click layer → assert inspector shows it) and `drag-clip.json` (drag a clip → assert moved rect + non-overlap held). Gate (positive): both scripts exit 0; deliberately-broken selector exits non-zero with candidates listed; drag script's dump shows the clip's new rect. Gate (negative): section 6 deletion gate; `rg -n "Instant::now|SystemTime" crates/manifold-app/src/ui_snapshot/` → zero hits. **Acceptance demo (L2, section 10):** `result.json` plus the numbered PNGs from both proving scripts — the landing reviewer looks at the drag-clip run's before/after PNGs and confirms the clip visibly moved. 2026-07-05 note: `interact.rs` has grown ~10× since the section 6 inventory was baked — the seam brief's re-derivation command is mandatory before any edit there. Test scope: `-p manifold-ui --lib`, `-p manifold-app --features ui-snapshot` builds, script runs. No workspace sweep.
- **P3 — Live primary window. IN PROGRESS 2026-09-06.** Implementation and focused tests pass on `codex/live-ui-control`; native-user interruption acceptance and landing remain (BUG-m7nb; gate blocked by existing Downloads fixture access). Current types, limits, acceptance and deferred scope are in the live contract above.
- **P4 — Flows + docs. IN PROGRESS 2026-09-06.** `scripts/live_ui_generator_demo.py` completed the live generator workflow, including undo/redo and playback checks, in 3.68 seconds after cursor-handover hardening. Disconnect/undo recovery passed; the diagnostic-enforced safety flow still needs its final live run. Other flow families and repeatability at alternate window sizes remain future work.

## 10. Decided — do not reopen

1. Selector surface = the extended tree dump; no separate semantic tree (D1).
2. Targets resolve by identity at act time; coordinate scripts forbidden where a widget target exists (D2).
3. One `AutomationAction` enum; headless runner and opt-in Unix live connection reuse it. Unsupported live verbs fail explicitly.
4. Headless injects at `UIRoot::pointer_event`/`key_event`; live injects at the `window_input.rs` dispatchers; no OS-level event synthesis (D4).
5. Hit-test ⇒ register: custom surfaces implement `HitTargets` or the feature owning them is incomplete (D5).
6. No silent fallbacks; misses fail loudly with the dump attached (D6).
7. Script owns the clock; deterministic stepping in headless mode (D7).
8. Names are `&'static str`; row identity via structural query, never per-row name allocation (D8).
9. Live connection: explicit feature and private socket path; no TCP or new threads. The normal single-instance app lock remains in force.
10. Assertions: the four D10 checks; pixel goldens stay deferred.
11. The automation layer has zero mutation verbs; all effects flow through real input → existing command lanes (section 7, section 8).

## 11. Deferred (with revival triggers)

- **Golden-image regression gate** — revive when the visual design locks (unchanged from `HEADLESS_UI_HARNESS.md`).
- **Generator-correct thumbnails in headless graph scenes** — owned by the harness doc's existing follow-up (drive `GeneratorRenderer`), not this design.
- **MCP `ui` tool group** (product-grade agent-assists-user driving) — revive when MCP v1 has shipped AND a user-facing need exists; forwards to `AutomationAction` per section 7.
- **3D gizmo targets** — land with REALTIME_3D's viewport phases via the D5 rule; nothing to build here now.
- **Perform-surface flow library** — write the flows when PERFORM_SURFACE P1 lands; the substrate (this design) is ready for them.
- **Recording/trace of a live session as a replayable script** — revive if hand-authoring flows proves tedious in practice.
