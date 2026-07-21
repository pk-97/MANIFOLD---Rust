# Landing: UI_FUNNEL_DECOMPOSITION P-B — DispatchCtx + chain-router dispatch split

**Date:** 2026-07-21 · **Wave:** god-file Wave 1, WS1 · **Branch:** `lane/ws1-bridge` → main
**Executors:** 01BALc8 dispatcher loop (Sonnet lanes per slice, per `docs/AGENT_ROUTING.md` §Overnight) after the ws1-projection Opus seat's opening arc (ctx struct, sentinel, browser pilot). Landing review + merge: 012PAn top session. **Design:** `docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md` P-B (D6 as amended: chain router, no delegation arms).

## What landed

- `DispatchCtx` (D3): `dispatch(action, ctx)` replaces the 18-arg signature; scrub slots regrouped verbatim (die at P-I).
- `dispatch_inspector` (was one ~3,160-line fn, 150 variants): now a 36-line ordered first-non-unhandled chain over `dispatch/{browser,clip,params,modulation,mapping,audio_setup}.rs` + `resolve.rs`; `inspector.rs` = router + cross-domain test corpus (2,435 lines incl. tests).
- Verifier hardened through six tooling commits (scaffold class, use-block tracking, drifted-preamble sequence match, router terminal); self-test suite unified with WS2's: **16/16 green**.
- Queue/decisions protocol files (D-17..D-24) — the overnight orchestration record.

## Verification (independently run by the landing session)

- Move identity, every pure-move commit: S1 `4d271d97`, S2 `1f5c9247`, S3 `c1740f2f` (2,358 moved, scaffold 7/25), S4 `4bad469e` (scaffold 7/25), S5 `fb59db17` (13/25), S6 `5c1cb982` (4/25) — all `PURE MOVE PROVEN`, residue 0.
- Census: 150 distinct `PanelAction::` variants across `dispatch/` + router — equals entry census; S9 LIST diff empty.
- INV-G6: one `fn dispatch(` in `ui_bridge/mod.rs`. INV-G7: router-only `dispatch_inspector` read and confirmed.
- Oracle suites + chain completeness: `65 tests run: 65 passed` (undo_baseline / mapping_undo_baseline / bug_266 / dispatch_chain_completeness). Full `manifold-app`: `319 tests run: 319 passed, 3 skipped`. Clippy `-p manifold-app --tests` clean.
- Full gate + flow runner at push time: see addendum below.

## Shortcuts taken

None in landed code. Process notes: mid-phase dual-dispatcher collision on slot-3 resolved by D-23 (no work lost, verified); D-11 byte-exact preambles carried as scaffold in 3 domain modules — they are the P-I scrub wire's first deletion targets.

## Click-script for Peter (≤2 min)

1. Scrub any layer-effect slider mid-playback → value follows, exactly one undo entry (the whole dispatch path now runs ctx + chain).
2. Right-click the same slider → reset to default, undo restores (SliderReset recursion through the new signature).
3. Map a param to a macro from the mapping chevron → mapping appears, undo removes it (mapping domain module).
4. Toggle an audio-mod driver on a card row → badge lights, drawer opens (modulation domain module).
