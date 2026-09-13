Branch: codex/rt-smaller-tiles. Base: 296355b01. Commits: b0ce61e07, 062cc8b21.

Latest commit: 75bd207d0 adds diagnostic MANIFOLD_GI_PROBE modes. First user
comparison: diagnostics=1, MANIFOLD_GI_PROBE=first-bounce, same 4 GI samples and
export range. This removes only the second GI bounce. no-sun and
first-bounce-no-sun are available for subsequent isolation; their rendering
has not yet been exercised. Omitting the variable restores normal GI.
GPU-crate clippy, gi_probe configuration test, and the focused
rt_bug17r3_lightless_gi proof with first-bounce mode passed. Release rebuilt.
Logs: /tmp/manifold-gi-first-bounce-proof.log, /tmp/manifold-gi-probe-release.log.

Quarter query budget (1 << 22) plus all five uncommitted slot-0 readiness files,
transferred at Peter's explicit request. The five readiness files were verified
byte-identical to slot-0 after release build. Slot-0 was not changed.

RUSTC_WRAPPER= bypassed the prior sccache permission error. Renderer clippy
--tests -D warnings, pending_declaration_reaches_consumers_and_resets, release
manifold-app build, and all three rt_bug318_import_toggle GPU proofs passed.
User's project loaded but still hung on GI=4 with the combined readiness/tile
build. The new first-bounce probe has not yet been tested on that project.
Executable: target/release/manifold in this slot. No main landing attempted;
experimental branch push previously blocked by the app-delivery hook.

2026-09-09: Peter STOPPED bounds-check work after finding a previously successful Right Where I Need You export now fails. Uncommitted raytrace.rs and GPU architecture docs contain bounds instrumentation. Focused GPU clippy and trace_hit_bounds_rejects_invalid_reads (native API/shader validation enabled) PASSED. Real GI proof gate interrupted by user direction (exit130), NOT verified; release NOT rebuilt. Do not present current executable as containing bounds checks. New regression evidence: existing MP4 modified Sep4 10:40 local, 16.271s 1080x1920@24; latest session1788924803792-51431 loads v5 ALT (not supplied V5), first-bounce-no-sun still enabled, export beats271-303 fails frame47. Need controlled known-good version comparison, no further speculative sample toggles.
