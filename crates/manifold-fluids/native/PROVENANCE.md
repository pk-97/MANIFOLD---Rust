# FLIP Fluids native provenance

The vendored engine is FLIP Fluids 1.8.8 at upstream revision
`70a0e954018fe39e1f9c3631264989569752bb7a`.

These files are copied byte-for-byte from the pinned upstream
[engine source](https://github.com/rlguy/Blender-FLIP-Fluids/tree/70a0e954018fe39e1f9c3631264989569752bb7a/src/engine):

- all top-level engine `.cpp` and `.h` files selected by the upstream `SOURCES_FLUID_ENGINE_LIBRARY` list
- `pcgsolver/*.h` headers used by the pressure solver
- `mixbox/mixbox.h`
- `mixbox/mixbox_stub.cpp`
- `versionutils.cpp.in` (instantiated by `build.rs` into `OUT_DIR`)

`bridge.cpp` and `bridge.h` are MANIFOLD-owned code. The bridge compiles with
`WITH_MIXBOX=0`, so no external Mixbox runtime or download is required.
The native mutex serializes all engine operations because upstream thread and
mesh-source counters are mutable process-global state.

The upstream MIT license text is preserved in `LICENSE_MIT.md`; each vendored
source file also retains its original license header.
