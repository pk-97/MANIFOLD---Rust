# Validation Playbook

Use the smallest meaningful validation command while iterating, then broaden when confidence or impact warrants it.

## Fast Path

- Single crate edit: `./.codex/bin/validate.sh <crate-name>`
- Workspace sanity: `./.codex/bin/validate.sh`
- Need tests too: `./.codex/bin/validate.sh <crate-name> --tests`

## Suggested Targets

- `manifold-core`: data models, registries, math, serializers used by core types
- `manifold-editing`: commands, editing service, undo/redo flows
- `manifold-io`: load/save/migration and archive logic
- `manifold-playback`: scheduler, sync, transport, clip lifecycle
- `manifold-renderer`: effects, generators, render targets, WGSL wiring
- `manifold-app`: UI bridge, app lifecycle, integration glue

## Runtime Bugs

When the issue smells like event ordering, callback timing, or mutable state drift:
- add logs quickly
- reproduce
- read the logs before redesigning anything

## Reporting

Always say what you ran.

If you could not run validation, say why.
If validation was targeted instead of full-workspace, say that explicitly.
