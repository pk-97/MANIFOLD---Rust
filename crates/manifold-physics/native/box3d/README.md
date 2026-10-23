# Box3D provenance

The `include/`, `src/`, and `LICENSE` files in this directory are vendored
from https://github.com/erincatto/box3d at commit
`8441b4a06d6d09dcfb0b0f704df4d847d1437b92` (the pinned v0.1.0 checkout).

`bridge.c` is the small C ABI boundary used by `manifold-physics`; it calls
only the public Box3D headers and does not expose upstream structs to Rust.

The local `b3Body_GetExternalAccelerations` extension in `include/box3d/box3d.h`
and `src/body.c` exposes queued external linear and angular acceleration through
the bridge's body dynamics snapshot. These values combine the body's current force and
torque with world gravity and inverse inertia without waking the body,
consuming queued values, or attempting to predict damping, gyroscopic, or
contact responses.

The mesh contact triangle query (`src/mesh_contact.c`, `b3RefreshCache`) no
longer stops at a fixed 256 triangles (upstream `B3_MAX_MESH_CONTACT_TRIANGLES`).
It collects every triangle under the query bounds into a growable per-worker
array, `meshTriangleQuery` on `b3TaskContext` (`src/physics_world.h`, created
and destroyed in `src/physics_world.c`), and takes its matching contact-cache
scratch from the worker's arena. Upstream dropped the triangles past 256, so a
body resting on a dense mesh lost the ones it touched and sank.
