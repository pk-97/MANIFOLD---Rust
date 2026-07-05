# PARAM_STORAGE P1 (V1.4 id-keyed params wire + quarantined migration) — landed 2026-07-05 @ db546564

**Branch:** wave/param-storage-p1 (merged `--no-ff`; merge parents cdf37515 + branch-tip 186df124) · **Level reached:** L1 (focused tests + fixture load) / target L1 (§10 — P1 changes the wire + migration only; storage semantics unchanged, so no L2 render artifact is applicable this phase)
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — **P1 SHIPPED @ `c7ae831f` (2026-07-05)**: V1.4 id-keyed wire + quarantined migration landed, positional arms deleted from effects.rs; P2–P5 remain (P2 = the compile-driven storage swap).`

> **Retroactive report (written 2026-07-05, late-session).** This landing's original chat-side report was produced earlier in the same session and has since been summarised out of context. Structural facts (merge SHA, parents, scope, the deviation) are recovered from git and the phase notes and are accurate. The **verbatim gate-output tails are NOT recoverable** — the section below reconstructs the gate that was run and its intent from notes, and is explicitly marked as reconstructed rather than pasted. Written per DESIGN_DOC_STANDARD.md §8.10's retroactive-preservation directive so the click-script and deviation survive.
>
> Note the SHA split: the doc status line quotes `c7ae831f` (the P1 content commit on the branch); the merge into `main` is `db546564`. Both name the same landing.

## Gate results (verbatim)

**Reconstructed from phase notes — original verbatim tails unavailable (see banner).** P1 test scope per the design (§P1) was FOCUSED, not a workspace sweep, because P1 changes only the serde wire + a quarantined migration and leaves in-memory storage semantics untouched:

- `cargo test -p manifold-core --lib` — green (V1.4 `params`-map serialize/deserialize round-trips; `base` rides the entry under `base_tracked`).
- `cargo test -p manifold-io --lib` — green (the `param_storage_v14.rs` `Value → Value` migration: positional `paramValues` array → id-keyed `params` map, baked id tables).
- Canonical fixture-load tests — green (`Liveschool Live Show V6 LEDS.manifold` and the migration-chain fixtures load byte-exact through the new migration).
- `cargo clippy` on the touched crates — clean.

Negative gate (positional wire arms deleted): `rg` for the typed positional symbols across `crates/` returned **0** outside the migration module.

## Deviations from brief

- **Negative-gate glob under-exclusion (resolved, not a shortcut).** The brief's negative `rg` used `-g '!*/migrations/*'` to exclude the migration module. Ripgrep's `*` does not cross `/`, so that glob under-excludes and the literal string `baseParamValues` still matched at `crates/manifold-io/src/migrations/.../migrate.rs:700`. Investigated: that hit is a **legitimate migration-chain key** (the migration must name the old field to read it), and every *typed* positional symbol (the deleted structs/fns) is genuinely zero everywhere. The gate's intent — no positional arms survive in live code — was satisfied; the surviving hit is required migration data. Corrected mental model of the glob for later phases (P2's negative gate uses `-g '!**/migrations/**'`).
- **origin/main moved mid-phase.** `origin/main` advanced to `cdf37515` (Peter's docs commit) while P1 was in flight. Merged `origin/main` into the branch (docs-only, 0 conflicts), re-ran the focused gate, then landed `--no-ff` at `db546564`. This is the standard fetch/merge/gate/merge loop, not a deviation from process — recorded because the merge parent (`cdf37515`) in the history is otherwise unexplained.

## Shortcuts confessed (rolled up from phase reports)

none.

## Verification debt

none opened, none carried. (P1 is fully covered by the focused unit + fixture-load gate; there is no runtime/UI surface in a wire-format + migration change that headless tests can't reach.)

## Click-script for Peter (≤2 minutes)

1. Open a **pre-V1.4** project file (any saved before this landing — its effects store a positional `paramValues` array) — expect: it loads without error and every effect/generator card shows its saved slider values (the V1.4 migration ran on load, mapping positions → ids).
2. Nudge one slider, then **Save** the project and reopen the `.manifold` in a text editor — expect: that effect now serialises a `"params": { "<id>": { "value": … } }` **map** (id-keyed), not a positional array, and no top-level `baseParamValues` array.
3. Load the canonical `Liveschool Live Show V6 LEDS.manifold` — expect: loads clean, LED show renders as before (byte-exact migration; this is the load-bearing migration fixture).
