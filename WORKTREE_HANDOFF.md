# Unlanded RT profiler instrumentation

Source commit: d46163e48, base e8a180d85. Adds acceleration-structure timestamp sampling using the existing Metal sampler, GPU kind totals and existing RT update counters in project diagnostic reports. Does not change dirty checks, geometry classification or playback policies.

The bounded project capture verified positive AS samples, zero invalid samples, zero failed command buffers, zero overflow and reconciled kind totals plus signed residual. The sampled work still does not explain the large entrance stall. All five reported worst frames rebuilt three BLAS; necessity and stall causality are not established. Investigation: BUG-dl16. The independent dirty-signal refactor should not be duplicated here.

Feature-enabled clippy passed. Required landing checks passed except one full GPU suite failure: rt_bugmajv_kernel_toggle::rt_kernel_toggle_sequence_preserves_raster_base, mean_abs_diff 0.0716 at line311. GPU proof binary had 216 passes, one failure; no drifted goldens. All other test binaries passed. The same exact isolated test subsequently passed on both unchanged base and this branch. Full-suite failure remains unresolved, tracked as BUG-3ckx; isolated passes do not make the landing gate green.

This archive is not an app landing. Resolve the suite-order/shared-state uncertainty before landing through scripts/land_branch.py. Use env RUSTC_WRAPPER= for builds; compiler-cache repair is deferred. Local capture, retained release binary and complete gate logs are in /tmp/manifold-corrosion-rt-attribution-20260921. No further project capture was run after the single instrumentation verification.
