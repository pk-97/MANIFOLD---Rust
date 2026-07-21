# Landing: UI_FUNNEL_DECOMPOSITION P-P — projection split + scratch rider

**Date:** 2026-07-21 · **Wave:** god-file Wave 1, WS1 · **Branch:** `lane/ws1-projection` → main
**Executor:** Opus seat (tmux window `ws1-projection`); landing review + merge: Fable top session. **Design:** `docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md` P-P.

## What landed

- `51993a13` — `state_sync.rs` (4,177 lines) → `ui_bridge/projection/{transport,timeline,inspector,cards,scene}.rs` along existing function boundaries; `push_state` stays the per-frame entry in `state_sync.rs`. **Pure move PROVEN:** `scripts/move_identity_check.py 51993a13` → `moved lines: 7044  allowlisted wiring: 58  comment lines: 1  visibility pairs: 10  residue: 0`.
- `91e9aad0` — rider, own commit (D10/INV-W4): `param_slots_to_ui` fresh-`Vec`-per-call → `with_param_slots` thread-local scratch (`clear()`+`extend`, slice to closure); old fn deleted, all 4 call sites converted, no parallel path. Reviewed line-by-line by Fable: no shared state (thread-local), reentrancy would panic loudly (RefCell), capacity bounded by largest manifest.
- `6a29d23e` — two doc comments still naming the deleted fn fixed; `move_identity_check.py` learned visibility-pair + comment classification (class found during this review: `fn` → `pub(crate) fn` widening is required wiring for cross-module moves; verifier now proves it instead of a human waiving 21 residue lines). Self-tested: widening move → PROVEN; smuggled `y*2`→`y*3` edit → caught.

## Gates

- INV-G1 move identity: quoted above, residue 0.
- INV-G5 (`rg 'content_tx' projection/` → no hits): PASS.
- Deletion gate (`rg 'param_slots_to_ui'` → comment references only, all updated): PASS.
- Full sweep at landing (warm main checkout): recorded below at push time.
- INV-G4 frame-cost trace: **carried** — see Verification debt.

## Verification level

**L1+L3 (partial)**: full workspace suite + flow subset at landing (results quoted in the push-time addendum below). The P-P surface is value-sync plumbing; layout unchanged (pure move), rider asserted behavior-identical by the untouched card/scene value suites.

## Verification debt

- VD: INV-G4 `MANIFOLD_RENDER_TRACE=1` interactive frame-cost spot-check not yet run on the rider path (headless flows exercise the same sync calls but not the multi-window per-frame cadence). Burn down: next interactive session on this build, or the P-F landing whose gates run the trace anyway. Entered in `docs/VERIFICATION_DEBT.md`.

## Shortcuts taken

None in the landed code. Process deviation, recorded honestly: the executing seat died (machine restart) after its final commit; its own gate claims (`clippy -p manifold-app` clean, `nextest -p manifold-app` 317 passed, per commit message) were not independently observed by the orchestrator — the landing-time full sweep supersedes them.

## Click-script for Peter (≤2 min)

1. Open any project with a layer effect → inspector card values update live under playback (projection path).
2. Scrub any card slider mid-playback → value follows, exactly one undo entry (rider path, `undo_baseline` contract).
3. Open the scene panel on a 3D layer → properties rows show live values (scene projection path).

## Push-time addendum (gates run by the orchestrating session, warm main checkout)

- Full sweep post-merge: `Summary [171.697s] 3850 tests run: 3850 passed (16 slow), 13 skipped`; `cargo clippy --workspace --tests -- -D warnings` exit 0; `cargo deny check bans` → `bans ok`.
- Flows: **17/17 green** — all 15 `scene-setup-*` (14 `gltfscene`, `empty-states` `timeline`), `drag-clip` (`timeline`), `select-and-inspect` (`inspector`). One false FAIL during the run: `select-and-inspect` was first invoked under the wrong scene (`timeline`) from lore-based mapping — flow→scene mapping is not machine-readable. The P-B brief carries the fix as a deliverable: `scripts/ui-flows/manifest.json` + `run_ui_flows.py` so the BUG-252 count-match gate becomes mechanical.
- Level reached: **L3** on the flow subset above; full-suite count-match deferred to the manifest (this landing's subset was chosen by documented mapping, not post hoc).
