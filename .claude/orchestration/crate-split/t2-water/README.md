# T2 water crate move

Move the existing water adapters, primitives and their water-specific proof
helpers to manifold-nodes-water. Also move the native vector-field and particle
duration primitives from image, and the volume-surface primitives from scene.
The catalog owns the mixed image/water particle pipeline test.

The source bodies and shaders are unchanged except for declared path rewrites.
Module visibility and cfg attributes are preserved. The old empty water mount,
build-script wiring, Cargo.lock, catalog link, test mounts and enforcement-path updates are
separate reviewed residue; they must be completed before landing.

The replay is not a completed extraction. Its templates retain the existing
module attributes and feature gates; the manifest hunks transfer native
dependencies and forward the existing water proof features. Source hashing
moves to the owning crate's build script. No external dependency version changes.

Commit this plan before the pure move. Verify that commit with both replay and
move identity checks, then review compiler-derived boundary fixes separately.
Compare the default and gpu-proofs test censuses with the pre-move inventories
and complete the required T2 behavioral and landing gates for the combined tree.
