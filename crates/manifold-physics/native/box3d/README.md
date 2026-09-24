# Box3D provenance

The `include/`, `src/`, and `LICENSE` files in this directory are vendored
from https://github.com/erincatto/box3d at commit
`8441b4a06d6d09dcfb0b0f704df4d847d1437b92` (the pinned v0.1.0 checkout).

`bridge.c` is the small C ABI boundary used by `manifold-physics`; it calls
only the public Box3D headers and does not expose upstream structs to Rust.

The exact-mesh drop experiment patches `src/mesh_contact.c` to cache queries
in mesh-local coordinates and retain all candidate mesh triangles using the
existing reusable arena. This supports testing a moving scan against a fixed
convex floor. Mesh CCD remains unsupported.
The bridge also enables `b3MeshDef::preserveSmallTriangles` so the mesh builder
retains positive-area scan detail smaller than the usual collision tolerance.
Moving meshes bypass the terrain contact-recycling shortcut: their contact
normals and supporting triangles must be recomputed as they rotate.
Moving mesh-versus-hull contacts also use two-sided finite triangles instead
of terrain's face-direction restriction. The terrain restriction could choose
a triangle plane reporting metres of overlap against a large floor even while
the triangle was above it. Static terrain retains its original behavior.
This prototype does not qualify moving meshes against capsules or spheres.

`src/mesh_pair_contact.c` adds two-sided mesh/mesh surface contacts. A dual BVH
walk rejects distant regions; GJK evaluates the original triangle pairs.
Contact points are grouped by normal and reduced while retaining the deepest
point, then passed to the existing Box3D solver. Warm starting matches both
triangle indices and nearby anchors. No collision proxy or triangle reduction
is used. This is discrete surface contact, not a solid-volume containment test;
outer steps must keep relative surface motion within the speculative margin.
The BVH query margin includes rest clearance and estimated relative linear and
angular motion over one outer step, capped by the native speculative distance.
The flower example explicitly uses 120 Hz contact stiffness and 960 Hz fragment
steps; the native world defaults remain unchanged for other callers.
Moving mesh/hull contact reduction also retains the deepest contact and clusters
by contact normal; static terrain keeps the upstream reduction path.
