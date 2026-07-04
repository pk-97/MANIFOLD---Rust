# Timeline Layout P0.1 — after evidence

Generated 2026-07-04 against `docs/TIMELINE_LAYOUT_P0_SPEC.md`'s P0.1 phase (D1 +
D2 + D3), via the same headless `ui-snap` harness the P0.0 before-set used
(`docs/HEADLESS_UI_HARNESS.md`). Every command below is byte-identical to the
before-set's (`docs/evidence/timeline_p0/before/README.md`) — same scenes, same
seeds, same interactions — so any pixel difference is attributable to the P0.1
code change, not a changed repro recipe. All commands assume cwd = repo root
(or worktree root); re-run via `env -C <worktree>` if driving a worktree so
output lands in that worktree's own `target/`.

| PNG | Command | What it shows |
|---|---|---|
| `01-baseline.png` | `cargo xtask ui-snap timeline --dump` | Unmodified 7-layer fixture. **Byte-identical to the before-set** (verified via `shasum -a 256`) — this scene never exercised the scroll/collapse paths D1–D3 touch, so no diff is expected. |
| `02-collapsed-group.png` | `cargo xtask ui-snap timeline --interact "collapse:bg-stack"` (`.after.png`) | **Byte-identical to the before-set.** Confirms P0.0's finding that this scene never reproduced RC2 headless (`sync_build` always fully resyncs) — nothing here for P0.1 to change. |
| `03-post-delete.png` | `cargo xtask ui-snap timeline --interact "delete:flowers"` (`.after.png`) | **Byte-identical to the before-set.** Same reasoning as above. |
| `04-audio-expanded.png` | `cargo xtask ui-snap timeline --scroll 300 --interact "select:kick"` (`.after.png`) | **Byte-identical to the before-set.** KICK is still the last layer, so nothing overflows into a neighbor here — RC3/D4 is out of this phase's scope (P0.2). |
| `05-audio-collapsed.png` | `cargo xtask ui-snap timeline --scroll 300 --interact "collapse:kick"` (`.after.png`) | **Differs from the before-set** (expected: D3's rebuild-time re-clamp changes the resting scroll position slightly for this fixture). RC3 (the Gain/Send controls spilling below the collapsed card) is **unchanged and still present** — D4 is P0.2's job, not this phase's. Header and lane columns agree on Y for every row; only the audio card's internal content still overflows its own row. |
| `06-shrunk-content-while-scrolled.before.png` | `cargo xtask ui-snap scrollshrink --dump` (base, unscrolled) | **Byte-identical to the before-set** — reference fixture, untouched by this interaction. |
| `06-shrunk-content-while-scrolled.png` | `cargo xtask ui-snap scrollshrink --scroll 5000 --interact "collapse:stack-2"` (`.after.png`) | **RC1 is fixed.** Scrolled to the bottom, then LAYER 2 collapses (content shrinks). Compare to the before-set: there, an orphan lane clip-strip rendered with no header above it, and every header/lane pair sat at a visibly different vertical offset. Here, the header column and lane column show the exact same set of rows in the exact same order at the exact same Y — the previously-orphaned top lane strip now has its matching header directly above it, because `rebuild_mapper_layout` (D3) re-clamps the one shared scroll position (D2) the instant the mapper rebuilds, and the header panel draws from that same value plus the same `CoordinateMapper` (D1) the lanes use — there is no second copy left to drift. |

## Regeneration note (shared output path)

`cargo xtask ui-snap timeline ...` and `cargo xtask ui-snap scrollshrink ...`
both write to a single stable path per scene
(`target/ui-snapshots/<scene>/<scene>[.after].png`) — each invocation
overwrites the previous one. To produce this set, each command was run and its
output copied to this directory *before* running the next command (scenes
01–05 all use the `timeline` scene and would otherwise clobber each other).

## Verification performed

- `shasum -a 256` on all 5 scenes unaffected by the scroll path (01–04, 06
  base) confirms byte-for-byte identity with the before-set — the fix has zero
  visual side effect where none was expected.
- Scenes 05 and 06 differ from the before-set (D3's re-clamp legitimately
  moves the resting scroll position); 06 was inspected visually and confirms
  the header/lane realignment described above.
- The stronger, mechanical version of this same claim is the two new unit
  tests added in P0.1 (`-p manifold-ui --lib`):
  `panels::viewport::tests::rebuild_mapper_layout_reclamps_scroll_immediately`
  (D3) and
  `panels::layer_header::tests::header_rows_agree_with_mapper_y_collapsed_hidden_child_group`
  (D1) — PNGs are the visual proof, the tests are the regression gate.
