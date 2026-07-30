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
`crates/manifold-renderer/src/preset_runtime/tests/`. This MUST be CPU-only —
`binding_seed.rs` is a STYLE reference only (imports, how it builds a chain): it lives
under `#[cfg(all(test, feature = "gpu-proofs"))]` in `mod.rs`, and its `crate::test_device()`
call doesn't even compile without that feature. A test dropped next to it inherits the
same gate and is invisible to the default `cargo nextest run` this program's gates use —
that's exactly how a previous version of this program went wrong. Wire your new test the
way `bug080_manifest_gate.rs` and `topology_hash.rs` are wired instead: a plain
`#[cfg(test)] #[path = "tests/your_file.rs"] mod your_file_tests;` block in `mod.rs`,
no `feature = "gpu-proofs"`.

Build the graph device-free: `PrimitiveRegistry::with_builtin()` +
`crate::node_graph::graph_loader::instantiate_def` (or `Graph` directly) need only the
registry, not a device — `PresetRuntime::try_build` and `crate::test_device()` both
require `&GpuDevice` and are gpu-proofs-gated, so neither belongs in this test.
`apply_binding_defaults` and `BoundGraph::new` are themselves device-free (they operate
on `&mut Graph`), so the whole repro — stamp a binding, instantiate the def, mutate the
live node's param, rebuild via `BoundGraph::new` again, assert the mutation was clobbered
— never needs a GPU. If you get partway in and find some step genuinely can't be done
without a device, stop and report that finding instead of routing around it with a
feature gate.

The failure message must name what was expected and what was found, so that when the fix
lands the test reads as a description of the contract rather than a puzzle.

## Files

You have the paths, not their contents. Open them yourself:
{{path:crates/manifold-renderer/src/node_graph/bound_graph.rs}}
{{path:crates/manifold-renderer/src/node_graph/scene_exposure.rs}}
{{path:crates/manifold-renderer/src/node_graph/graph_loader.rs}}
{{path:crates/manifold-renderer/src/preset_runtime/mod.rs}}
{{path:crates/manifold-renderer/src/preset_runtime/tests/bug080_manifest_gate.rs}}
{{path:crates/manifold-renderer/src/preset_runtime/tests/binding_seed.rs}}

Start by reading `apply_binding_defaults` in `bound_graph.rs` and
`stamp_scene_node_exposures_into` in `scene_exposure.rs`. The mechanism is in those two
functions and the `instantiate_def` call between them.

## Bounds

Touch only test files, plus the one `mod` declaration `mod.rs` needs to wire your new
file in (the same single line every other test file there already has). Do not change
`bound_graph.rs`, `scene_exposure.rs`, or `graph_loader.rs` — the fix is a separate slice
under full-context review, because this binding lifecycle is load-bearing and a previous
change to it caused a regression.

Do not weaken, skip, or `#[ignore]` any existing test to make room.

Commit your own work with a pathspec naming the exact files. Never `git add -A`.
