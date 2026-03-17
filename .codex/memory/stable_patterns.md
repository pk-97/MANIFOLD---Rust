# Stable Patterns

This file is for durable repo-specific findings that future Codex sessions should reuse. Keep entries short, concrete, and stable.

## Current Notes

- The root `AGENTS.md` is the authoritative Codex contract in this repo; `.codex/` is the reusable support layer, not the source of truth.
- `app.rs` and `ui_bridge.rs` are the first places to check for accidental inline service logic before porting a Unity service.
- The workspace already has user edits in flight sometimes; never assume a clean tree, and do not revert unrelated changes.
- Use targeted crate validation during iteration; run broader checks when the change crosses crate boundaries.
