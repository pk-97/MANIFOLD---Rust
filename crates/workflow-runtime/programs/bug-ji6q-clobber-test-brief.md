# Write the BUG-ji6q regression test — and prove it RED

You are writing a test that MUST FAIL on the current tree. Do not fix the bug. A green
test here is a failed step: it would mean the test does not reproduce what the bead
describes.

## The bug, as diagnosed

Card-stamped defs — GLB imports, via `stamp_scene_node_exposures_into` — give every node
param a `BindingDef` whose `default_value` is a frozen snapshot of `node.params` at stamp
time.

On every graph build, `instantiate_def` correctly writes the def's `node.params` onto the
live node. Then `BoundGraph::new` calls `apply_binding_defaults`, which unconditionally
replants every binding's `default_value` over the top.

Two copies of one fact, nothing keeping them in sync. Any writer that touches
`node.params` for a bound target — def edits, migrations, direct macro or mapping writes —
is silently reverted at the next rebuild.

The per-frame `ParamManifest` path (`bound.apply`) is IMMUNE: live macro sliders are fine.
It bites def edits, migrations, and direct `node.params` writers. Your test must exercise
the path that breaks, not the one that works.

A CPU-only reproducer is known to work: post-assembly def mutation of a `bake_environment`
intensity param, then a survives-rebuild check.

## What to write

One test named exactly:

    bound_param_write_survives_a_graph_rebuild

Shape: build or load a graph with a card-stamped binding, mutate a bound param AFTER the
stamp, rebuild, and assert the mutated value survived. It will not survive — that is the
point, and the assertion is what makes the failure legible.

Put it where the nearest existing binding regression tests live:
`crates/manifold-renderer/src/preset_runtime/tests/` — read `binding_seed.rs` first and
match its style, its imports, and how it builds a chain. Wire the test into the module
tree properly; an orphaned file that nothing includes will fail the gate.

CPU-only if you can. If the reproducer genuinely needs a device, use the same
`crate::test_device()` the neighbouring tests use.

The failure message must name what was expected and what was found, so that when the fix
lands the test reads as a description of the contract rather than a puzzle.

## Files

You have the paths, not their contents. Open them yourself:
{{path:crates/manifold-renderer/src/node_graph/bound_graph.rs}}
{{path:crates/manifold-renderer/src/node_graph/scene_exposure.rs}}
{{path:crates/manifold-renderer/src/node_graph/graph_loader.rs}}
{{path:crates/manifold-renderer/src/preset_runtime/tests/binding_seed.rs}}

Start by reading `apply_binding_defaults` in `bound_graph.rs` and
`stamp_scene_node_exposures_into` in `scene_exposure.rs`. The mechanism is in those two
functions and the `instantiate_def` call between them.

## Bounds

Touch only test files. Do not change `bound_graph.rs`, `scene_exposure.rs`, or
`graph_loader.rs` — the fix is a separate slice under full-context review, because this
binding lifecycle is load-bearing and a previous change to it caused a regression.

Do not weaken, skip, or `#[ignore]` any existing test to make room.

Commit your own work with a pathspec naming the exact files. Never `git add -A`.
