# Box3D provenance

The `include/`, `src/`, and `LICENSE` files in this directory are vendored
from https://github.com/erincatto/box3d at commit
`8441b4a06d6d09dcfb0b0f704df4d847d1437b92` (the pinned v0.1.0 checkout).

`bridge.c` is the small C ABI boundary used by `manifold-physics`; it calls
only the public Box3D headers and does not expose upstream structs to Rust.

The exact-mesh drop experiment patches `src/mesh_contact.c` to cache queries
in mesh-local coordinates and retain all candidate mesh triangles using the
existing reusable arena. This supports testing a moving scan against a fixed
convex floor. Mesh-versus-mesh collision and mesh CCD remain unsupported.
The bridge also enables `b3MeshDef::preserveSmallTriangles` so the mesh builder
retains positive-area scan detail smaller than the usual collision tolerance.
Moving meshes bypass the terrain contact-recycling shortcut: their contact
normals and supporting triangles must be recomputed as they rotate.
Moving mesh-versus-hull contacts also use two-sided finite triangles instead
of terrain's face-direction restriction. The terrain restriction could choose
a triangle plane reporting metres of overlap against a large floor even while
the triangle was above it. Static terrain retains its original behavior.
This prototype does not qualify moving meshes against capsules or spheres.
