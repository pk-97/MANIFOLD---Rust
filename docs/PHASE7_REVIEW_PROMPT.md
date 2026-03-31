# Phase 7 — Post-Migration Review and Cleanup

Read `CLAUDE.md` before starting.

The codebase was migrated from wgpu to native Metal across 6 phases by multiple agents. Each phase was scoped narrowly. This review cleans up the accumulated debris: dead code, stale comments, unused imports, inconsistent patterns, and leftover transition artifacts.

**Rules:**
- Fix each issue as you find it. Do not batch analysis — fix, then move on.
- Do NOT change rendering logic, shader code, or public APIs.
- Do NOT rename functions or restructure modules.
- Only remove code that is provably dead (no callers, no references).
- `cargo clippy --workspace -- -D warnings` must pass after every batch of changes.
- Commit and push when done.

Work through each task in order. Mark each done before proceeding.

---

## Task 1: Remove Dead wgpu/glyphon References

Run these searches. For each match, determine if it's dead code or a legitimate reference (e.g., in a comment explaining history). Remove dead code. Leave historical comments only if they explain WHY something works the way it does.

**Search 1:** `grep -rn "wgpu" crates/manifold-renderer/src/ crates/manifold-app/src/`
- Remove any `use wgpu::` imports
- Remove any `wgpu::` type references
- Remove stale comments like "wgpu Device", "wgpu Queue", etc. that describe removed code
- Do NOT touch files outside manifold-renderer and manifold-app

**Search 2:** `grep -rn "glyphon" crates/manifold-renderer/`
- Should return zero results. If any remain, remove them.

**Search 3:** `grep -rn "Phase [0-9]" crates/manifold-renderer/src/ crates/manifold-app/src/`
- Remove comments like "Phase 3 transition", "Phase 5 conversion", "Phase 6 replaces this"
- These were migration breadcrumbs, no longer useful

**Search 4:** `grep -rn "TEMPORARY\|HACK\|FIXME\|TODO\|SKIP\|transition\|placeholder" crates/manifold-renderer/src/ crates/manifold-app/src/`
- Evaluate each match. Remove if the TODO is done or the transition is complete.

**Search 5:** `grep -rn "native_device\|gpu\.device\b" crates/manifold-app/src/`
- After Phase 6, `GpuContext` has only `device: GpuDevice` (no more `native_device` vs `device` distinction). Any reference to `native_device` is stale — it should be `gpu.device`.

Build after this task: `cargo clippy --workspace -- -D warnings`

---

## Task 2: Remove Dead Files and Modules

Check if these files/modules still exist and are still referenced:

- `crates/manifold-renderer/src/blit.rs` — should be deleted (replaced by native blit in app.rs)
- `crates/manifold-renderer/src/surface.rs` — should be deleted (replaced by GpuSurface)
- `crates/manifold-renderer/src/panel_compositor.rs` — should be deleted (replaced by native atlas blit)
- Any `pub mod blit;`, `pub mod surface;`, `pub mod panel_compositor;` in `lib.rs` — remove

If any of these still exist, delete them. If they're already gone, skip.

Check `crates/manifold-renderer/Cargo.toml`:
- `wgpu` should NOT be in dependencies
- `glyphon` should NOT be in dependencies
- `wgpu-hal`, `wgpu-types` should NOT be in dependencies

Check `crates/manifold-app/Cargo.toml`:
- `wgpu` should NOT be in dependencies
- `wgpu-hal`, `wgpu-types` should NOT be in dependencies
- `pollster` — check if still used anywhere. If not, remove.

Build after this task.

---

## Task 3: Remove Dead Fields and Unused Imports

Read each file below. For each, check for:
- Unused `use` imports (clippy catches most, but check manually)
- Struct fields that are written but never read
- Methods that are defined but never called (check with grep)
- `#[allow(dead_code)]` attributes — verify the code IS actually used. If not, remove the code and the attribute.

Files to check:
1. `crates/manifold-app/src/app.rs` — look for fields related to the old wgpu pipeline (blit_pipeline leftovers, old surface fields, intermediate target textures that were removed)
2. `crates/manifold-app/src/app_render.rs` — look for commented-out code blocks, unused variables
3. `crates/manifold-app/src/shared_texture.rs` — check if `import_texture()` (wgpu version) still exists. It should be removed. Only `import_texture_native()` should remain.
4. `crates/manifold-app/src/output_presenter.rs` — check for leftover PresenterThread code
5. `crates/manifold-renderer/src/ui_renderer.rs` — check for leftover glyphon fields, TextMode enum (is it still used?)
6. `crates/manifold-renderer/src/native_text.rs` — check for unused imports, dead helper functions

Build after this task.

---

## Task 4: Comment Accuracy

Read these files and fix comments that describe behavior that no longer matches the code:

1. `crates/manifold-app/src/app_render.rs` — the render pass comments (Pass 1, Pass 2, etc.) must match the ACTUAL order. After the compositor/atlas swap, verify the comments match.
2. `crates/manifold-app/src/output_presenter.rs` — module-level doc comment should describe the current architecture (inline blit, not dedicated thread)
3. `crates/manifold-renderer/src/ui_cache_manager.rs` — doc comments should reference GpuTexture/GpuEncoder, not wgpu
4. `crates/manifold-renderer/src/layer_bitmap_gpu.rs` — module doc comment should reference manifold-gpu, not wgpu

Do NOT add new comments. Only fix existing ones that are wrong.

---

## Task 5: Verify LoadAction Consistency

Read `crates/manifold-app/src/app_render.rs` and trace through every render pass in order. For each `draw_fullscreen`, `draw_fullscreen_viewport`, `draw_indexed`, `clear_texture`, and `ui_renderer.render()` call, verify:

1. The first operation on the drawable clears it (either `clear_texture` or `LoadAction::Clear`)
2. Every subsequent operation uses `LoadAction::Load` (preserves previous passes)
3. No operation uses `LoadAction::DontCare` on the drawable (would discard previous work)

If any LoadAction is wrong, fix it. Document what you changed and why.

Also check `crates/manifold-gpu/src/metal/encoder.rs`:
- `draw_fullscreen()` with `clear=false` should use `MTLLoadAction::Load` (not DontCare)
- `draw_fullscreen()` with `clear=true` should use `MTLLoadAction::Clear`
- Verify this is currently correct (it was fixed during debugging)

---

## Task 6: Verify Cargo.toml Workspace Dependencies

Check `Cargo.toml` (workspace root):
- If `wgpu`, `wgpu-hal`, `wgpu-types` are in `[workspace.dependencies]`, check if ANY crate still uses them
- If no crate uses them, remove from workspace dependencies
- Check if `glyphon` is in workspace dependencies — remove if unused
- Check if `pollster` is in workspace dependencies — remove if unused

Build after this task.

---

## Task 7: Final Verification

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. `cargo build --release 2>&1 | tail -5` — release build must succeed

---

## Task 8: Update Migration Doc

Update `docs/NATIVE_METAL_UI_MIGRATION.md`:
- Mark Phase 7 as `[DONE]`
- Add a summary at the top: "Migration complete. All UI rendering uses native Metal via manifold-gpu. wgpu removed from manifold-renderer and manifold-app."

Commit and push all changes.
