# Standing decisions — god-file Wave 1 (append-only; team-lead writes, everyone reads)

Rule for all agents: BEFORE pausing on any fork, re-read this file — if your question is answered here, proceed; if not, PARK it in `parked.md` and continue with the next queue item. Chat messages are for nudges and reports; rulings live here.

- **D-1** Router = ordered first-non-unhandled chain; NO per-variant delegation arms (a hand-written variant→module table is a parallel routing copy — forbidden by name).
- **D-2** `DispatchResult.unhandled: pub(crate) bool`, true only via `unhandled()`; sole consumer is the router chain. Zero struct-literal construction outside the module (grep-gated).
- **D-3** Dispatch body references `ctx.X` directly (approach A). No per-module reborrow preambles; allowlisting bindings is forbidden (a wrong-field bind is a silent state reroute).
- **D-4** Verifier scaffold class: narrow patterns only, cap 25/commit, NEVER any `PanelAction::` pattern. Routing correctness is proven by census equality + chain-completeness, never by allowlist.
- **D-5** Variant-census equality == 150 after every slice; at phase end the sorted variant LIST diff vs entry census must be empty.
- **D-6** `dispatch_chain_completeness` invariant: one chain call per handler module file (mod.rs, resolve.rs exempt); rides nextest.
- **D-7** Pure-move and semantic changes NEVER share a commit. Lane never lands; top session merges behind the full gate.
- **D-8** Removed `#[allow(dead_code)]` lines are legitimate residue when the allow's named un-suppression trigger fired — name it in the commit message (precedent: browser slice, sentinel allow).
- **D-9** A slice's tiny (<3-line) genuine moves that fall below git's move threshold: name them in the commit message; reviewer eyeballs at landing. Keep that surface minimal.
- **D-10** Order within P-B: S2 (resolve.rs) lands before any preamble domain (S3–S5) because the helpers are shared (86 call sites across domains).
- **D-11** Preamble domains (params/modulation/mapping): the ONLY sanctioned preamble is byte-exact:
  `let (effective_tab, effective_active_layer) = super::editor_dispatch_context(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), ctx.active_layer);`
  `let active_layer = &effective_active_layer;`
  The first such slice adds these two lines as EXACT-STRING (whitespace-normalized) scaffold patterns to `move_identity_check.py` + one self-test fixture, its own tooling commit. Any deviation from the byte-exact form = residue = investigate, never adapt.
- **D-12** Overnight scope fence: P-B remainder (this queue) and, if it completes AND the top session has landed it, P-F pure-move slices only. P-D/P-I/P-S do NOT start overnight.
- **D-13** Command hygiene for auto-approval (Peter 2026-07-21): phrase commands so the auto-mode classifier reads them as non-destructive — plain single-purpose commands; no `$()` writes, no redirects into repo paths, no destructive-git shapes; `/tmp` and `/dev/null` redirects are fine. A blocked command = park the slice, don't retry variants.
- **D-14** Every full workspace build/test/flow run goes through `.claude/scripts/with-build-lock.sh` — one heavy build machine-wide, no exceptions (two GUI lockups on 2026-07-21 correlate with concurrent sweeps).
- **D-15** (top session, WS2 P-F2a) Bare inherent-impl wrapper lines (`impl Type {` + bare closing brace) are ALLOW-class wiring in `move_identity_check.py` — an impl-block move into a submodule needs exactly one; wrappers alone carry no behavior (bodies still classify as moves/residue). Trait impls (`impl X for Y`) stay residue — escalate/park if genuinely needed. Verifier change ships with a 6th self-test case (wrapper move PROVEN; body edit inside wrapper CAUGHT), own tooling commit, flagged as widening the shared gate.
- **D-16** (top session) WS2 scope fence stands: ui_root.rs only until P-B lands on main; then P-F1/app.rs slices re-brief against the landed DispatchCtx call sites.
- **D-17** Orchestration-file topology (fixes the queue-location snag): the dispatcher merges `origin/main` into the lane ONCE (standard pre-landing merge, brings the kit), then OWNS `ws1-queue.md` on the LANE — checkbox commits ride the lane (pathspec), restart-proof, no top-session dependency; the top session freezes its main-side queue copy (lane version wins at landing). `decisions.md` stays MAIN-only, top-session-written; the dispatcher reads it FRESH from the main checkout absolute path (/Users/peterkiemann/MANIFOLD - Rust/.claude/orchestration/decisions.md) before pausing on any fork — never a lane copy. `parked.md` is created and committed on the LANE (seat-owned state).
