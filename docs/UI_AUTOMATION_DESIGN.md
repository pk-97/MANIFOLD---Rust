# UI Automation — the agent drives the instrument

**Status:** IN PROGRESS · **P1 SHIPPED 2026-07-05 @ `3294eb9d`** (selector surface). · **P2 SHIPPED 2026-07-05** (script driver: `AutomationAction` core + selector resolver + real gesture synthesis incl. a genuine synthesized clip drag through the production input path + `--script` runner + `interact.rs` miss-fallback deleted; gate green, L2 reached — the drag-clip flow moved a clip 230→314px in the before/after PNGs — see §9 P2). **L3 verification is now available repo-wide** via `scripts/ui-flows/` (see `DESIGN_DOC_STANDARD.md` §10). P3 (live door) + P4 (flow library) not built. · 2026-07-03 · Fable · baseline-reviewed 2026-07-05 (anchors spot-reverified; automation-lane surface added to P1/D5 scope; acceptance demos + §10 levels added; dev feature renamed `ui-automation`)
**Prerequisites:** none. P1–P2 extend the shipped ui-snap harness; P3–P4 are self-contained dev infra. No wave edges (`docs/DESIGN_BUILD_ORDER.md`).
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase. P1–P2 are full-hardened against today's code; P3–P4 carry pre-flight re-derivation commands.

Peter, 2026-07-03: *"we will likely need custom infra so you can interact with Manifold and test UI and UX features in depth"* — and, on scope: the agent should *"interact with the app as a first class feature… widgets, gizmos, etc"*, because *"this will be a huge help in verifying features and systems where unit tests can't. Automated integration testing!"*

The governing insight: **Playwright works because the DOM gives it three things — find, act, wait. MANIFOLD's bitmap UI already has two-thirds of the substrate**: a real retained tree with durable widget identity (`WidgetId`), a headless harness that renders the real UI and drives one real click, and a proven input seam. This design finishes the triad: the tree dump becomes the selector surface (the "DOM"), a gesture driver acts by widget identity through the production input path, and explicit sync replaces auto-wait. One interaction core, two transports: the headless harness (scripted, deterministic) and a dev-only live door into the running app.

Companion docs: `docs/HEADLESS_UI_HARNESS.md` (the shipped harness this extends — read it whole before P1) · `docs/MCP_INTERFACE_DESIGN.md` (the product AI surface; §9 pins how it may later forward to this layer — this design is NOT part of it) · `docs/INPUT_IDENTITY_UNIFICATION.md` (why `WidgetId` exists and how input tracks it).

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
| Interaction driver (seed) | `ui_snapshot/interact.rs:18` (`apply`) | SHIPPED, two verbs (`select:<layer>`, `open:settings`). Proves the seam: resolve rect from built tree → `UIRoot::pointer_event` Down+Up → `drain_events` → real `Panel::handle_event` dispatch. **Has a silent fallback on miss (`interact.rs:62-67`) — removed in P2 (§6 seam brief).** |
| UIRoot injection points | `crates/manifold-app/src/ui_root.rs:989` (`pointer_event`), `ui_root.rs:1011` (`key_event`) | SHIPPED. Take logical position / key + a caller-supplied `time: f32` — the clock is already a parameter, which is what makes deterministic scripting possible. |
| Live input dispatchers (one owner, both windows) | `crates/manifold-app/src/window_input.rs:103` (`input_cursor_moved`), `:118` (`input_mouse_input`), `:134` (`input_mouse_wheel`), `:1517` (`input_keyboard`) | SHIPPED. The single entry per winit event; window routing + scroll normalization + cursor projection live here. |
| Custom hit-test surfaces | `crates/manifold-ui/src/graph_canvas/hit.rs:60` (`hit_test`) — nodes/ports/wires; timeline clips via `crates/manifold-ui/src/clip_hit_tester.rs` | SHIPPED. These targets are invisible to `UITree::hit_test` and to the dump — the P1 registration gap (§5). |
| Live sync primitives | `crates/manifold-app/src/content_state.rs:62` (`ContentState.data_version`), `crates/manifold-ui/src/tree.rs:56` (`structure_version`) | SHIPPED. The wait-condition substrate for the live door (§7). |
| Event loop | `crates/manifold-app/src/main.rs:112` (`EventLoop::new()` — no user-event type), `app.rs:1628` (`ApplicationHandler`), `app.rs:2533` (`about_to_wait`) | SHIPPED. No proxy/wakeup plumbing exists yet; P3 adds it. |
| Request/reply channel shape | `docs/MCP_INTERFACE_DESIGN.md` §3 (`McpRequest { kind, reply }`) | DESIGNED, not built. §4 reuses the *shape* (per-request bounded(1) reply channel), not the crate. |

Re-derivation (run at any phase start; if counts differ from above, stop and re-inventory):
`rg -n "fn hit_test" crates/manifold-ui/src/` · `rg -n "pointer_event|key_event" crates/manifold-app/src/ui_root.rs` · `rg -n "pub\(crate\) fn input_" crates/manifold-app/src/window_input.rs`

**Baseline-review addendum (2026-07-05, anchors spot-reverified):** all audited symbols
still exist; line numbers have drifted (`pointer_event` 989→1106, `key_event` 1011→1128,
`UITree::hit_test` 577→613, `widget_to_node` 48→53, `node_for_widget` 820→858, the
`input_*` dispatchers +13 each) — trust the re-derivation commands, not the baked numbers.
Two substantive changes since the audit: (1) **a new custom hit-test surface shipped
2026-07-04** — automation lanes (`crates/manifold-ui/src/automation_hit_tester.rs`,
`hit_test_automation` / `AutomationLaneScreen`) — added to the D5/P1 scope in §5;
(2) **`interact.rs` grew ~10×** (automation-lanes + preset-picker verification work);
the §6 seam brief's baked inventory is stale — its re-derivation command is now mandatory
before P2 touches the file (the miss-fallback currently sits near `interact.rs:608`).

## 2. Decisions

- **D1 — The tree dump is the selector surface.** The extended dump (WidgetId + component name + custom-surface targets, §3/§5) is the one machine-readable description of "what is on screen"; the agent navigates it like a DOM and every selector resolves against it. Rejected: an AccessKit-style separate semantic tree — a second structure to keep in sync with the real one, when the real one is already walkable and already carries text, type, hierarchy, and state flags.
- **D2 — Act by identity, resolved to coordinates at act time.** A script targets a widget (by name/text/structure query, §3); the driver resolves its rect from the *current* build and synthesizes input at that point. Rejected by name: **coordinate scripting** ("click at (412, 87)") — it rots on every layout change and is the tempting shortcut every executor will reach for. A raw `point:` target exists (§4) for empty-canvas cases only; a script that uses `point:` where a widget target exists fails review.
- **D3 — One transport-agnostic core, two transports.** A single `AutomationRequest` enum (§4) serviced on the UI thread. Transport A: the ui-snap script driver (headless, P2). Transport B: a dev-only localhost server (live, P3). Rejected: building this into `manifold-mcp` v1 — that couples a dev instrument to wave-3 product work and its tokio runtime; the MCP server may later grow a *gated* `ui` tool group that forwards to this same enum (§9 forward constraint), which is why the enum, not the transport, is the contract.
- **D4 — Injection enters at the proven seams, one per mode.** Headless: `UIRoot::pointer_event`/`key_event` (`ui_root.rs:989/1011`) + real panel `handle_event` dispatch — exactly the seam `interact.rs` proved. Live: the `window_input.rs` dispatchers (`input_*`), so window routing, scroll normalization, and cursor projection all run. Rejected by name: **OS-level event synthesis (CGEvent/AppKit)** — needs a window-server session, can't run headless, races the real cursor, and tests the OS instead of MANIFOLD.
- **D5 — The hit-test ⇒ register rule.** Any surface that answers its own hit-testing (graph canvas, timeline clips, future 3D gizmos) implements `HitTargets` (§5) and appears in the dump. A new interactive surface that doesn't register is incomplete by definition — this is what makes the agent first-class rather than "can click some things". Peter's scope quote above is the mandate.
- **D6 — No silent fallbacks, ever.** A target that doesn't resolve, or a synthesized gesture that misses, fails the script loudly with the dump attached as evidence. The existing `interact.rs` miss-fallback (`interact.rs:62-67`) is deleted in P2. (House rule: `feedback_no_silent_fallbacks_or_interim_stopgaps`.)
- **D7 — The script owns the clock.** Headless runs pass explicit time into `pointer_event`/build; a `step` action advances frames by fixed dt. No wall-clock reads in the driver. Same run → same pixels → same dump, every time.
- **D8 — Names are `&'static str` component names; dynamic identity comes from structure.** Panels name interaction points with static strings (`"layer_header.mute"`); *which row* comes from the selector's ancestor query (§3), not from allocating per-row name strings. The editor rebuilds its tree every frame — per-node `String` names would be a per-frame alloc on the UI thread. Hot-path rule wins; `Vec<Option<&'static str>>` costs nothing.
- **D9 — The live door ships in dev builds only.** Feature `ui-automation` (off by default, like `ui-snapshot`; renamed from `automation` in the 2026-07-05 baseline review — the automation-lanes UI shipped 2026-07-04 and owns the bare word in this codebase now), std-TCP + JSON-lines on `127.0.0.1`, port only via explicit `--automation-port <n>`. No tokio, no auth token: the feature is compiled out of shipping builds, so the venue laptop never has the surface at all. Rejected: a second product server (duplicates MCP's job); a bearer token (ceremony for a dev-only, loopback, opt-in flag).
- **D10 — Minimal assertions in the script driver; pixel goldens stay deferred.** `assert` steps cover exists / text-equals / count / rect-within (§6). Everything richer is the reading agent's job over the emitted dumps. Golden-image diffing remains deferred exactly as `HEADLESS_UI_HARNESS.md` decided — a moving visual design would make it noise.

## 3. Selector model — the dump becomes the DOM

`dump_tree` (`ui_snapshot/dump.rs:12`) gains three fields per node, all additive:

- `widget`: the `WidgetId` raw value as hex (`node.rs:347` `raw()`), emitted for interactive nodes. The durable handle a script acts on.
- `name`: the static component name (D8), when registered. Registration API: a `name: Option<&'static str>` parameter on the keyed/interactive node builders in `tree.rs` (exact plumbing free to the executor; storage is `Vec<Option<&'static str>>` alongside `widget_ids`, `tree.rs:34`).
- `targets`: for nodes owning a custom surface — the `HitTargets` enumeration (§5).

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
/// (headless) and the dev TCP server (live) both compile scripts down to this.
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
    /// Emit the extended dump (§3) to the run's output dir / reply.
    Dump,
    /// Emit a PNG of the current UI to the run's output dir / reply.
    Snapshot,
    /// D10 assertion; failure = loud stop with dump attached.
    Assert { selector: AutomationTarget, check: AssertCheck },
}

pub enum AutomationTarget {
    Query(SelectorQuery),          // §3 structural query
    Widget(u64),                   // a WidgetId raw value from a prior dump
    Surface { surface: &'static str, kind: String, label: String }, // §5
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

`cargo xtask ui-snap <scene> --script <file.json>` — a JSON array of §4 actions, executed in order against the scene fixture. Artifacts land in `target/ui-snapshots/<scene>/run-<script-stem>/`: numbered PNGs and dumps at each `Snapshot`/`Dump` step, plus `result.json` (per-step outcome, resolved targets, assert results). Exit 0 only if every step succeeded (D6, D10).

The `select:`/`open:` `--interact` verbs become sugar for one-step scripts; `interact.rs`'s dispatch rewires to the §4 core.

**Seam brief — `interact.rs` miss-fallback removal (P2):**
- Old: on synthesized-click miss, warn and fall back to direct id match (`interact.rs:61-67` — the WARNING eprintln plus `clicked.unwrap_or(idx)`).
- New: a miss returns a step failure carrying the dump; no fallback path exists. Delete the fallback arm, not just the warning.
- Call-site inventory: `interact::apply` has exactly one caller (`ui_snapshot/mod.rs:116`). Re-derive: `rg -n "interact::apply" crates/manifold-app/src/` — if >1, stop and list.
- Deletion gate: `rg -n "fell back|unwrap_or\(idx\)" crates/manifold-app/src/ui_snapshot/` → zero hits.

Determinism (D7): the driver owns a monotonically stepped clock; `Step` advances it by `frames × dt` at the fixture's fixed dt. ⚠ VERIFY-AT-IMPL: where time currently enters `UIRoot` build/animation (the `time: f32` on `pointer_event` is caller-supplied; confirm no other wall-clock reads on the headless path — `rg -n "Instant::now|SystemTime" crates/manifold-app/src/ui_snapshot/ crates/manifold-ui/src/`).

## 7. The live door (dev builds only)

The piece that makes the agent a user of the *running* instrument — real content thread, generators animating, transport running, both windows up.

- **Transport:** feature `ui-automation` in `manifold-app`. A std `TcpListener` thread on `127.0.0.1:<port>` (only when `--automation-port` is passed), speaking JSON-lines: one request per line, one reply per line. No tokio (D9). Precedent for the thread-with-channel shape: the MCP design's request lane (`MCP_INTERFACE_DESIGN.md` §3), shape reused, crate not shared.
- **Threading:** requests cross to the UI thread via a bounded(8) crossbeam channel + `EventLoopProxy` wakeup. `main.rs:112` changes to `EventLoop::with_user_event::<AutomationWake>()` (or `()` as pure wake — executor's choice, both are winit-supported); the handler services **at most one automation request per event-loop turn** in the `user_event` arm. The reply travels on the request's own bounded(1) channel. Zero new shared state (hard rule).
- **Injection:** live requests compile to calls on the `window_input.rs` dispatchers (D4) — the same functions winit events enter through, with `WindowTarget` picking the workspace.
- **Sync verbs (live-only additions):** `WaitFor { condition, timeout_ms }` where condition ∈ { `DataVersionAtLeast(u64)` (`content_state.rs:62`), `SelectorExists(query)`, `StructureVersionChanged` (`tree.rs:132`) }. Timeout = loud failure with dump (D6). This is the auto-wait leg of the triad.
- **Screenshot, live:** ⚠ VERIFY-AT-IMPL — the UI presents to a winit drawable; a live `Snapshot` needs a readback seam (render the UI pass to an offscreen texture and blit, or copy before present). Read `ui_snapshot/render.rs` and the present path in `app_render.rs` before designing the copy; the harness's readback (`render.rs`) is the precedent. Program-output snapshot (the IOSurface front buffer) is deliberately NOT duplicated here — that is MCP's `get_output_snapshot` (`MCP_INTERFACE_DESIGN.md` §7); if it is needed for automation before MCP ships, escalate rather than parallel-build.
- **Content mutations:** none via this surface. The automation layer drives *input*; whatever the input causes flows through the existing `ContentCommand`/`EditingService` lanes like any user gesture. The live door adds no mutation verbs of its own (that separation is what keeps its security story trivial).

**Forward constraint (pinned):** if the product MCP surface ever exposes UI driving (agent-assists-user flows), it does so by forwarding to `AutomationAction` over this same channel, gated by its own product-grade auth — the enum is the contract, transports are swappable. Nothing in MCP v1 does this; the pin only prevents a future second action vocabulary.

## 8. What does NOT change

- `EditingService` stays the sole mutation gateway; automation mutates nothing directly — it produces input events.
- The two-thread model is untouched. The live door is a UI-thread *requester*; it never touches the content thread (its only content-thread contact is reading `ContentState` snapshots the UI already has).
- `UIInputSystem`, panel `handle_event` dispatch, and the winit dispatchers keep their exact behavior — automation enters through them, never around them.
- Shipping builds are byte-identical in behavior: both features (`ui-snapshot`, `automation`) are compiled out.

## 9. Phasing

Forbidden across all phases: coordinate scripting where a widget target exists (D2) · any fallback on miss (D6) · wall-clock in the headless driver (D7) · per-frame allocation for names or target enumeration (D8, §5) · a parallel "test-only" input path that bypasses `process_pointer`/panel dispatch (the whole point is exercising the real one).

- **P1 — Selector surface. ✅ SHIPPED 2026-07-05 @ `3294eb9d`** (L2 — editor/timeline/automation dumps read at landing: 107 graph targets with scope/node/port payloads, 9 clips with clip-id payloads, 7 automation strips/breakpoints, named transport + layer-header widgets; `cargo test -p manifold-ui --lib` 595/595; clippy clean). Landing note: the `custom_surfaces` enumeration is a sibling top-level dump key, not the per-node `targets` field the §3 prose implies — no `UITree` node owns the graph canvas / clip / automation surfaces (they're addressed by screen rects), so the enumeration is carried alongside `nodes`; still strictly additive. Minor gap → VD-005. `manifold-ui`: name storage + builder plumbing (D8), `HitTargets` trait + graph-canvas, timeline-clip, and automation-lane impls (§5); `manifold-app`: dump gains `widget`/`name`/`targets` (§3); naming pass at the §3 scope. Read-back: §3, §5, `dump.rs` whole, `graph_canvas/hit.rs` + `model.rs`, `automation_hit_tester.rs` whole. Deliverables: extended dump visible in `ui-snap editor --dump` and `timeline --dump`. Gate (positive): editor-scene dump lists every node/port the canvas `hit_test` can return, with payload ids; timeline dump lists every fixture clip and every automation-lane strip/breakpoint visible in the fixture; `cargo test -p manifold-ui --lib` green including new tests for name storage + a `HitTargets` enumeration test per impl. Gate (negative): `rg -n "String" crates/manifold-ui/src/` shows no per-node name `String` storage in `tree.rs` (names are `&'static str`). **Acceptance demo (L2, §10):** the two dumps above are the artifacts — the landing reviewer reads them and confirms named widgets, graph targets with payload ids, clips, and automation-lane targets are present; absence of any category is a gate failure, not a note. Test scope: `-p manifold-ui --lib` + the two ui-snap runs; no workspace sweep (additive dev surface, no product path touched).
- **P2 — Script driver. ✅ SHIPPED 2026-07-05** (L2 — both proving flows exit 0; drag-clip moved Plasma 1's clip 230→314px through the real `process_pointer`→`process_events`→`InteractionOverlay`→`AppEditingHost` path with 6 interpolated steps, before/after PNGs read at landing; `cargo test -p manifold-ui --lib` 604/604; clippy clean; D6 hard-failures verified — zero-match and ambiguous Pointer both exit non-zero with candidates; both negative gates zero hits). Landing notes: the enum lives in `manifold-ui`, which gained a `serde` dependency (workspace, for the JSON `--script` format the doc mandates — `AutomationTarget` uses a manual `Deserialize` that leaks the `Surface.surface` string to keep the doc's committed `&'static str` type); `AutomationAction::Text` has no headless injection seam and fails loudly (neither proving flow needs it); the headless drag routes clip mutations through a driver-held `crossbeam` channel whose receiver is never drained (`ContentCommand::send` only errors on disconnect), so the real mutation lands on the scene `Project` with no live content thread. `AutomationAction` enum in `manifold-ui` (§4 committed shape), gesture synthesis incl. interpolated drag, selector resolver, `--script` runner + artifacts + `result.json`, `--interact` rewired as sugar, `interact.rs` fallback deleted (§6 seam brief). Read-back: §4, §6, `interact.rs` whole, `ui_root.rs:989-1030`. Deliverables: two proving scripts committed under `scripts/ui-flows/`: `select-and-inspect.json` (click layer → assert inspector shows it) and `drag-clip.json` (drag a clip → assert moved rect + non-overlap held). Gate (positive): both scripts exit 0; deliberately-broken selector exits non-zero with candidates listed; drag script's dump shows the clip's new rect. Gate (negative): §6 deletion gate; `rg -n "Instant::now|SystemTime" crates/manifold-app/src/ui_snapshot/` → zero hits. **Acceptance demo (L2, §10):** `result.json` plus the numbered PNGs from both proving scripts — the landing reviewer looks at the drag-clip run's before/after PNGs and confirms the clip visibly moved. 2026-07-05 note: `interact.rs` has grown ~10× since the §6 inventory was baked — the seam brief's re-derivation command is mandatory before any edit there. Test scope: `-p manifold-ui --lib`, `-p manifold-app --features ui-snapshot` builds, script runs. No workspace sweep.
- **P3 — Live door.** Feature `ui-automation`, TCP JSON-lines thread, channel + `EventLoopProxy` wakeup, one-request-per-turn servicing, live injection via `input_*` dispatchers, `WaitFor` verbs, live `Snapshot` (resolve the ⚠ readback seam first — escalate if it needs a present-path change beyond a copy). Read-back: §7 whole, `window_input.rs:1-60`, `app.rs:2533` region, MCP design §3. Deliverables: `scripts/ui-flows/live-smoke.json` — connect, dump, click transport play, `WaitFor DataVersionAtLeast`, snapshot. Gate (positive): smoke script passes against a live run with a playing project; app with feature off has no listener (`lsof -i :<port>` empty). Acceptance demo (L4, §10 — manual live drill by design). Gate (negative): `cargo build -p manifold-app` (default features) then `rg -n "automation" target/` symbol check via `nm` on the binary → no automation server symbols; `rg -c "Arc<Mutex|Arc<RwLock" crates/` count unchanged from phase start. Test scope: focused; manual live drill is the gate. Pre-flight: re-run §1 re-derivation commands (this phase may execute months after P1).
- **P4 — Flows + docs.** A starter library of real regression flows under `scripts/ui-flows/` (MIDI-map a param, open graph editor and select a node via surface target, mute/solo matrix), `docs/HEADLESS_UI_HARNESS.md` updated to cover the driver, this doc's status flipped. Gate: each flow runs green twice consecutively (determinism check — this is the acceptance demo, L2 per §10); doc review by Peter. Test scope: script runs only.

## 10. Decided — do not reopen

1. Selector surface = the extended tree dump; no separate semantic tree (D1).
2. Targets resolve by identity at act time; coordinate scripts forbidden where a widget target exists (D2).
3. One `AutomationAction` enum; transports (xtask script runner, dev TCP server) compile to it; MCP may only ever forward to it (D3, §7 pin).
4. Headless injects at `UIRoot::pointer_event`/`key_event`; live injects at the `window_input.rs` dispatchers; no OS-level event synthesis (D4).
5. Hit-test ⇒ register: custom surfaces implement `HitTargets` or the feature owning them is incomplete (D5).
6. No silent fallbacks; misses fail loudly with the dump attached (D6).
7. Script owns the clock; deterministic stepping in headless mode (D7).
8. Names are `&'static str`; row identity via structural query, never per-row name allocation (D8).
9. Live door: dev-feature only, loopback, opt-in port flag, std TCP, no tokio, no token, compiled out of shipping builds (D9).
10. Assertions: the four D10 checks; pixel goldens stay deferred.
11. The automation layer has zero mutation verbs; all effects flow through real input → existing command lanes (§7, §8).

## 11. Deferred (with revival triggers)

- **Golden-image regression gate** — revive when the visual design locks (unchanged from `HEADLESS_UI_HARNESS.md`).
- **Generator-correct thumbnails in headless graph scenes** — owned by the harness doc's existing follow-up (drive `GeneratorRenderer`), not this design.
- **MCP `ui` tool group** (product-grade agent-assists-user driving) — revive when MCP v1 has shipped AND a user-facing need exists; forwards to `AutomationAction` per §7.
- **3D gizmo targets** — land with REALTIME_3D's viewport phases via the D5 rule; nothing to build here now.
- **Perform-surface flow library** — write the flows when PERFORM_SURFACE P1 lands; the substrate (this design) is ready for them.
- **Recording/trace of a live session as a replayable script** — revive if hand-authoring flows proves tedious in practice.
