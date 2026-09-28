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
