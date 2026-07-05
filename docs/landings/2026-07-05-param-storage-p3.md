# PARAM_STORAGE P3 (transport-block topology guard, D8) — landed 2026-07-05

**Branch:** wave/param-storage-p2 (merged `--no-ff` into main; the merge commit is the landing SHA — `git log --merges --first-parent main | head`) · **Level reached:** L2 (unit coverage of the exact reorder-misroute case + a control) / target L4 — the running-app confirmation that a live modulation display stays on the right slider when a neighbour is deleted mid-modulation is carried as VD-008 (headless tests cannot drive the live UI).
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — **P3 SHIPPED (2026-07-05)**: the UI↔content modulation bridge now stamps each transport block with ParamManifest::topology() at capture and skips a block on apply when the live topology no longer matches — replacing the len == len guard that silently misrouted a same-length param reorder. P4 (Ableton/OSC onto the manifest) + P5 (registry containment + library re-save) remain.`

## What changed

`crates/manifold-app/src/content_state.rs`, one file, +122/−5. `ModulationSnapshot` gains a `block_topos: Vec<u32>` array parallel to `block_lens`. `capture_into` pushes `manifest.topology()` for every param block (the macro block, which has no manifest, gets a never-consulted sentinel `0` and stays guarded per-slot). `apply` replaces all three `fx.params.len() == len` / `gp.params.len() == len` guards (master effects, layer effects, generator params) with `manifest.topology() == captured_stamp`. Same topology ⟹ structurally identical ⟹ the existing zip is length-safe, so the len comparison is **removed, not paralleled** (per the brief's forbidden move). No new allocation on the apply hot path — `block_topos` grows once with `block_lens` (capture side), and apply reads it with `.get(block).copied()`.

The bug this closes: `topology` bumps on every add/remove/reorder but never on a value write (pinned by `params.rs::same_length_reorder_changes_topology`). Delete a slider and add another (or reorder a card) between capture and apply and the two manifests were the same length, so the old guard wrote stale modulation values onto the wrong params — a live modulation display jumping to the wrong slider mid-show.

## Gate results (verbatim)

- `cargo test -p manifold-app --bins`: **130 passed; 0 failed; 2 ignored.** Includes the two new `content_state::modulation_topology_guard_tests`: `apply_skips_block_on_same_length_reorder` (the exact case the old guard missed — a `[a,b]`→`[b,a]` reorder is skipped, live values 0.90/0.80 survive instead of being overwritten by the stale 0.10/0.20 capture) and `apply_writes_block_when_topology_unchanged` (control — unchanged topology still applies captured values over live drift).
- `cargo clippy -p manifold-app --all-targets -- -D warnings`: **clean** (exit 0). `--all-targets` so the new `#[cfg(test)]` module is linted too (default targets skip test code).
- Negative grep (`\.len\(\) == len` in `content_state.rs`): **0 hits** — the approximate guard is gone.
- **Test scope is focused `-p manifold-app`, not a workspace sweep — this is the design's own P3 scope decision** ("Test scope: focused `-p manifold-app` + the smoke. No sweep."). Justified: the change is one manifold-app file, and origin/main's only advance since the P2 landing (fe363d86..999c9dfa) is a daemon `moves.md` doc, a merge commit, and glTF test fixtures — **zero Rust code**, none of it reachable from manifold-app.
- No `gpu-proofs` run: P3 touches no shader/kernel/uniform.

## Deviations from brief

- **The brief said `cargo test -p manifold-app --lib`; manifold-app is a binary crate with no lib target,** so the gate ran `--bins`. Same tests, correct target.
- **Merged origin/main (999c9dfa: glTF fixtures + a daemon `moves.md`) into the wave branch before landing, and re-gated the integrated tree** (`manifold-app --bins` green on the merge commit b24669fc). Clean 3-way merge, no conflicts — main added the fixtures, the branch never touched them.
- **The 128 MB glTF-blob "purge" rider from the P2 landing is now moot in the opposite direction.** origin/main deliberately committed `japanese_apricot`/`lowe` `.glb` as tracked fixtures at 999c9dfa, so they are canonical on main — a `filter-repo` purge would now *remove fixtures main wants*. Rider dropped.

## Shortcuts confessed

- **`block_topos` is a separate `Vec<u32>` parallel to `block_lens`, not a widened block header.** The brief left the layout to the executor ("`Vec<u32>` parallel to `block_lens`, or a widened block header — executor's choice, it's private layout"). The parallel-array shape keeps the change surgical (no re-encode of the existing `u16` lengths) and the two arrays stay 1:1 by construction.
- **The macro block carries a sentinel `0` topology it never reads.** Macros are `macro_bank.slots`, not a `ParamManifest`; their apply path is already per-slot `get_mut(i)` (inherently length-safe), so no topology guard is needed there. The sentinel exists only to keep `block_topos` index-aligned with `block_lens`/`block`.

## Verification debt

- **VD-008 opened** — P3 transport topology guard: running-app confirmation. The reorder-misroute is closed at unit level (the two `modulation_topology_guard_tests`), but the live path — a modulation display staying on the correct slider when a neighbour is deleted mid-modulation — is not exercised in a running app; headless tests have no live UI/modulation loop. Peter owns the L4 observation (click-script below).
- None carried from P1/P2 beyond the still-open VD-007.

## Click-script for Peter (≤2 minutes) — the P3 running-app smoke

1. Put an **LFO (or any driver) on a card slider** so its value is visibly moving.
2. On the **same effect/generator**, **delete a *different* slider** (a neighbour) while the modulation is running — this reorders/shrinks the param manifest for that instance.
3. **Expect:** the modulation stays on the *original* slider and its display keeps moving correctly; nothing jumps onto the wrong slider or freezes. (Before P3, deleting the neighbour left the same block length, so the stale modulation buffer wrote onto the wrong param.)
4. Bonus: **reorder** two params on a card mid-modulation (if the card supports it) — same expectation: the driver follows its param by identity, not position.
