# Box3D provenance

The `include/`, `src/`, and `LICENSE` files in this directory are vendored
unchanged from https://github.com/erincatto/box3d at commit
`8441b4a06d6d09dcfb0b0f704df4d847d1437b92` (the pinned v0.1.0 checkout).

`bridge.c` is the small C ABI boundary used by `manifold-physics`; it calls
only the public Box3D headers and does not expose upstream structs to Rust.
