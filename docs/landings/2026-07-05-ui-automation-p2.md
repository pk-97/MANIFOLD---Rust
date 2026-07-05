# UI_AUTOMATION P2 (script driver) — landed 2026-07-05

**Branch:** `wave/ui-automation-p2` (P2 content @ `f4ccfbca`, landed via the `--no-ff`
merge that commits this report) · **Level reached:** L2 / target L2 (§10)
**Doc status line (quoted verbatim):**
> **Status:** IN PROGRESS · **P1 SHIPPED 2026-07-05 @ `3294eb9d`** (selector surface). · **P2 SHIPPED 2026-07-05** (script driver: `AutomationAction` core + selector resolver + real gesture synthesis incl. a genuine synthesized clip drag through the production input path + `--script` runner + `interact.rs` miss-fallback deleted; gate green, L2 reached — the drag-clip flow moved a clip 230→314px in the before/after PNGs — see §9 P2). **L3 verification is now available repo-wide** via `scripts/ui-flows/` (see `DESIGN_DOC_STANDARD.md` §10). P3 (live door) + P4 (flow library) not built. · 2026-07-03 · Fable · baseline-reviewed 2026-07-05 …

Wave context: this is the second of two phases landed 2026-07-05. P1 (selector surface)
landed first @ `3294eb9d` (predates the §8.10 committed-report rule, so it has no own
file; its gate + demo are summarized in the P1 chat report and VD-005, now closed).

## Gate results (verbatim)

Re-run by the orchestrating session in the worktree at the merged tip (not
self-reported by the worker):

```
cargo test -p manifold-ui --lib
  test result: ok. 604 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

cargo clippy -p manifold-ui -p manifold-app --features manifold-app/ui-snapshot -- -D warnings
  Finished `dev` profile [unoptimized + debuginfo] target(s) in 47.80s   (clean)

# proving flow 1
ui-snap inspector --script scripts/ui-flows/select-and-inspect.json
  [ 2] ok  Assert text="GLOW" type="Label" — Exists
  [ 3] ok  Pointer text="PLASMA" type="Button" Click — acted at (111.0,1006.6)
  [ 4] ok  Assert text="PLASMA" type="Label" — Exists
  EXIT=0

# proving flow 2 (the drag)
ui-snap timeline --script scripts/ui-flows/drag-clip.json
  [ 2] ok  Assert Surface{timeline_clips,clip,"Plasma 1"} RectWithin — before rect (230.0,618.1 1152.0x188.0)
  [ 3] ok  Pointer Surface{timeline_clips,clip,"Plasma 1"} Drag{to:Point(902,712), steps:6} — acted at (806.0,712.1)
  [ 6] ok  Assert Surface{timeline_clips,clip,"Plasma 1"} RectWithin — after rect (314.0,618.1 1152.0x188.0)
  EXIT=0

# D6 hard-failures (run by orchestrator)
zero-match Pointer   → EXIT=1  "no match for query{text=NO_SUCH_WIDGET_XYZ}"  + FAILED dump written
ambiguous Pointer    → EXIT=1  "104 matches for query{type=Button} (need exactly 1): #2 Button … #222 Button …"

# negative gates (both ZERO hits)
rg "fell back|unwrap_or\(idx\)" crates/manifold-app/src/ui_snapshot/   → exit 1 (zero)
rg "Instant::now|SystemTime"    crates/manifold-app/src/ui_snapshot/   → exit 1 (zero)
```

Acceptance demo (L2): `target/ui-snapshots/timeline/run-drag-clip/{00,04}.png` read by
the orchestrator — the green **Plasma 1** clip starts flush at the left edge (beat 1)
in the before frame and sits shifted right (~beat 4, with a selection border) in the
after frame. A real, visible clip move through `process_pointer` → `process_events` →
`InteractionOverlay` → `AppEditingHost`, not a Down/Up teleport.

## Deviations from brief

- **`manifold-ui` gained a `serde` dependency** (workspace) + `serde_json` dev-dep. The
  §4 enum lives in `manifold-ui` and §6 mandates JSON `--script` files, so Deserialize
  on the enum is a logical necessity; putting it in `manifold-app` is doc-forbidden.
  serde is the repo-ubiquitous crate, not a novel one. Accepted as an operational call
  (crosses the "adding a dependency" escalation line, but within evident doc intent).
- **`custom_surfaces` dump shape (inherited from P1):** enumeration is a sibling
  top-level key, not the per-node `targets` field §3's prose implies — no `UITree` node
  owns those surfaces. Strictly additive; the P2 resolver keys off `surface_id`.
- **`AutomationTarget` uses a manual `Deserialize`** that leaks the parsed `surface`
  string via `Box::leak` to preserve the doc's committed `surface: &'static str` type.
  One tiny leak per script parse, fine for a one-shot dev tool.
- No coordinate/`Point` scripting used where a widget/surface target existed (D2 held);
  no silent fallback survived (D6); no wall-clock in the driver (D7).

## Shortcuts confessed (rolled up from phase reports)

- **`AutomationAction::Text` has no headless injection seam** — text editing lives in
  `Application::text_input`, unreachable from `UIRoot`; it fails loudly with a named
  reason. Neither proving flow uses it.
- **`apply_panel_actions` handles only `PanelAction::LayerClicked`** (mirrors
  `ui_bridge/layer.rs`); the full `ui_bridge::dispatch` needs `UserPrefs::load()` (disk
  I/O) which breaks D7 determinism. Other actions are logged, not silently dropped.
- **`drag-clip.json` uses PLASMA's clip** (alone on its layer) — the fixture's clip
  labels are generic ("Video N") and collide across layers, so a `Surface{label}` target
  for a "Video N" clip would be genuinely ambiguous. The drag exercises the real
  `enforce_non_overlap` path but does not prove collision resolution against a neighbor.
- **`select-and-inspect.json` asserts on the layer-chrome title label**, not effect-card
  presence — effect cards use a wall-clock-gated "dying" removal animation the headless
  `Step` clock can't drive deterministically.
- P1 rollup: name storage + three `HitTargets` impls; no stubs; the `custom_surfaces`
  placement above was P1's one design call.

## Verification debt

- **VD-005 CLOSED** (L3 reached) — the selector surface is now scripted-driven end-to-end.
- **VD-001 / VD-002 updated to runnable** — both were blocked on this wave; each now
  names the `scripts/ui-flows/` flow to write as its L3 burn-down.
- **None newly opened.** Organic-growth item carried (not debt): `editor`-scene widgets
  are unnamed headless — name as flows need them.

## Click-script for Peter (≤2 minutes)

This is dev/test infra — nothing new to click in the live app. To watch the automation
layer drive the real UI headless:

1. `cargo xtask ui-snap timeline --script scripts/ui-flows/drag-clip.json` — expect:
   prints steps `[0]…[6]` all `ok`, `EXIT=0`, and a line "after rect (314.0,…".
2. Open `target/ui-snapshots/timeline/run-drag-clip/00.png` then `04.png` — expect: the
   green **Plasma 1** clip is flush-left in `00`, shifted right (~beat 4, selected
   border) in `04`. The agent dragged it through the real input path.
3. `cargo xtask ui-snap inspector --script scripts/ui-flows/select-and-inspect.json` —
   expect: all steps `ok`, `EXIT=0` (resolve "PLASMA" button by text, click, assert).
4. (Optional, see a loud failure) hand it a bogus selector and confirm it exits non-zero
   with the candidate list — the D6 no-silent-miss guarantee.
