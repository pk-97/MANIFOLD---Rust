# Codex Workspace for MANIFOLD Rust

This directory is the committed Codex working set for this repo. It exists so future Codex sessions can start with repo-specific context, repeatable workflows, and a small amount of durable memory without touching the Claude setup.

## Start Here

1. Read `/Users/peterkiemann/MANIFOLD - Rust/AGENTS.md` fully
2. Read `/Users/peterkiemann/MANIFOLD - Rust/.codex/context/project_map.md`
3. For porting work, read `/Users/peterkiemann/MANIFOLD - Rust/.codex/context/porting_workflow.md`
4. For validation and closeout, read `/Users/peterkiemann/MANIFOLD - Rust/.codex/context/validation_playbook.md`
5. If the task is repeatable, open the matching template in `/Users/peterkiemann/MANIFOLD - Rust/.codex/prompts/`

## Directory Layout

- `context/`: short, stable repo knowledge that Codex should routinely reuse
- `prompts/`: reusable task frames for common MANIFOLD work
- `memory/`: durable findings worth carrying forward between sessions
- `bin/`: tiny helper scripts for fast scans and validation

## High-Value Commands

```bash
./.codex/bin/pre_port_scan.sh ProjectIOService
./.codex/bin/pre_port_scan.sh Assets/Scripts/UI/ProjectIOService.cs
./.codex/bin/validate.sh
./.codex/bin/validate.sh manifold-renderer
./.codex/bin/validate.sh manifold-app --tests
cargo test -p manifold-editing
cargo test -p manifold-io
```

## Working Rules for Codex

- Treat Unity source as the single source of truth for behavior and architecture
- Port services as units; do not scatter service logic inline into `app.rs`, `ui_bridge.rs`, or event handlers
- Prefer targeted `cargo check -p <crate>` during iteration, then broaden only when warranted
- When a runtime bug involves ordering, callbacks, or timing, instrument with logs quickly instead of over-analyzing statically
- If you learn a stable repo pattern that will help later Codex sessions, add it to `memory/stable_patterns.md`

## What This Is Not

- Not a replacement for `AGENTS.md`
- Not a copy of the Claude workspace
- Not a dumping ground for ephemeral notes, scratch output, or generated logs
