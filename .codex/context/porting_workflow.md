# Porting Workflow

This is the Codex-facing execution checklist for MANIFOLD port work. It is intentionally shorter than `AGENTS.md`; when in doubt, the root contract wins.

## Before Editing

1. Check `docs/PORT_STATUS.md`
2. Check `docs/KNOWN_DIVERGENCES.md`
3. Read the Unity source file completely
4. List dependencies and confirm they already exist as Rust units
5. Search for inline copies in `app.rs`, `ui_bridge.rs`, and nearby handlers
6. Use `./.codex/bin/pre_port_scan.sh <target>` to accelerate the scan

## Structural Mapping Checklist

- Fields mapped 1:1
- Methods mapped 1:1
- Interfaces preserved as traits
- Base classes preserved as trait plus shared-state pattern
- Service boundaries preserved
- Constants and defaults copied exactly
- Registry indices and uniform names copied exactly

## During Translation

- Keep logic order the same
- Preserve every guard and early return
- Match Unity math operations exactly
- Match texture formats exactly
- Avoid adding abstractions, generic wrappers, or extra state
- Prefer `unwrap()` / `expect()` when Unity would assume success

## After Translation

1. Re-read Unity and compare line by line
2. Run the narrowest useful validation command first
3. Update `docs/PORT_STATUS.md` if status changed
4. Update `docs/KNOWN_DIVERGENCES.md` only if a divergence is truly forced
5. Remove any old inline copy if the new service/module supersedes it

## Stop Conditions

- You have not read the Unity source
- The Rust code would flatten or merge Unity architecture
- A constant, format, or param index differs from Unity without explicit justification
- You are about to invent infrastructure instead of porting the actual Unity dependency first
