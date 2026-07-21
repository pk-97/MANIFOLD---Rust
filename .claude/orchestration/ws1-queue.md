# WS1 P-B slice queue — overnight run 2026-07-21

Dispatcher pops top-to-bottom; S1/S2 may swap freely; S3–S7 require S2 landed (helper paths). One Sonnet lane per slice, ONE commit, in slot-3 on `lane/ws1-bridge`. Gates per slice (all exit codes, quote outputs in the commit message): `python3 scripts/move_identity_check.py <commit>` · census `rg -o 'PanelAction::[A-Za-z0-9]+' crates/manifold-app/src/ui_bridge/dispatch/ crates/manifold-app/src/ui_bridge/inspector.rs | sort -u | wc -l` == **150** · `cargo clippy -p manifold-app --tests -- -D warnings` · `cargo nextest run -p manifold-app` (includes `dispatch_chain_completeness` + the three oracle suites). Any friction not covered by `decisions.md` → PARK (append to `parked.md`, skip slice, continue).

The proven recipe (browser slice, twice-validated) — per domain:
1. Move the domain's arms + DOMAIN-ONLY helpers VERBATIM to `ui_bridge/dispatch/<d>.rs`:
   `pub(crate) fn dispatch_<d>(action: &PanelAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult { match action { <arms>, _ => DispatchResult::unhandled() } }`
2. In `dispatch_inspector`: `let r = super::dispatch::<d>::dispatch_<d>(action, ctx); if !r.unhandled { return r; }` · in `dispatch/mod.rs`: `pub(crate) mod <d>;`
3. Run the four gates.

- [ ] **S1 clip** — arms inspector.rs:753–930 + `apply_detection_edit` (clip-only) + its imports. Preamble-free. READY.
- [ ] **S2 resolve** — pure helper move: dual-edit helpers (:61–223), resolvers (:223–341), `resolve_param_range`, `preset_source_def`, `audio_setup_command` → `dispatch/resolve.rs` (pub(crate)); 86 call sites keep working via use-wiring/path updates (wiring class in the verifier). No chain call, no census change; chain-completeness exempts resolve.rs already.
- [ ] **S3 params** — preamble domain (see decisions D-11).
- [ ] **S4 modulation** — preamble domain; heaviest helper user.
- [ ] **S5 mapping** — preamble domain.
- [ ] **S6 audio_setup**
- [ ] **S7 scene**
- [ ] **S8 flow-manifest rider** — `scripts/ui-flows/manifest.json` (every flow file → scene; select-and-inspect = `inspector`; scene-setup-* = `gltfscene` except empty-states = `timeline`; drag-clip = `timeline`; derive the rest from selectors vs `ui_snapshot/fixtures.rs`, list unresolvables — never guess) + `scripts/run_ui_flows.py` (reads manifest, runs each via `cargo xtask ui-snap`, per-flow PASS/FAIL + final count). Independent of S1–S7.
- [ ] **S9 phase close** — verify `dispatch_inspector` is router-only; full-lane `move_identity_check origin/main..HEAD`; census LIST diff vs entry census == empty (sorted variant names, not just count); report to team-lead for landing.

Landing is the TOP SESSION's job only. Dispatcher never merges to main, never pushes.
