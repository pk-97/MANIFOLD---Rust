User requested fixing Spectrogram crash and moving Oscilloscope/Spectrogram from effects to generators; explicitly no existing-project compatibility needed.

Work in codex/audio-visualizer-generators based on af08abc43fb26915de75d13ccb5608e66b61aa29. Archive reference to be added after worktree retirement. This is unfinished and not landed.

Confirmed crash cause: preset_runtime/build.rs assigned canvas dimensions to all transient texture slots despite AudioSpectrum declaring fixed 512x256 output. Implemented resolved-dimension allocation and same-size free-slot reuse. Two focused CPU allocator tests and renderer clippy passed. Production effect-chain GPU regression at 1080x1920 now passes.

Both preset JSON files moved to generator-presets, Amount/input-image mix removed, generator_input added, Oscilloscope aspect sourced from generator input, display transform output declared canvas size. Audio Send generator dropdown uses graph-bound values and undoable SetGraphNodeParamCommand. All 99 presets validate. Catalog regenerated.

Two focused GPU gate attempts: first failed due new test setup (no Oscilloscope fusion region after mix removal; unused source in effect fixture). Corrected those fixtures. Second: magnitude proof passed, portrait chain regression passed, generator proof rendered and compared Spectrogram raw/fused pixels successfully, then failed `spectrum covers the portrait canvas` at audio_visual.rs:272 (line may move).

Static cause of second failure: node.transform runtime uses shaders/affine_transform.wgsl, whose cs_main computes bounds and normalized UVs from textureDimensions(source_tex), despite dispatch_standalone_2d dispatching over the output. A 512x256 source displayed into 1080x1920 only writes the source extent. Resolve this at the actual sampling contract, retain same-size parity and fusion behavior, then verify full-frame output and source change/resize portions of generator proof that did not complete for Spectrogram. No third GPU attempt made under AGENTS.md two-failure cap.

Full renderer nextest was also run by worker and failed; exact failing names were lost in truncated output, so not classified as pre-existing. Run ID a580b89a-e9a5-4c0c-9c82-2d0d91a0135d. Later focused allocator tests passed; complete landing gate has not run.

Environment: default sccache returns Operation not permitted. Per-command RUSTC_WRAPPER= with escalated execution compiles successfully; no config changes. Use build lock for cargo/GPU work. Finish required clippy/tests/GPU/landing gate and land_branch.py before delivery. No user project was modified.

Final worker evidence:
- Generator projection and undo dispatch tests passed; app check --tests passed before final app_render dropdown edit. Final app_render integrates audioSend dropdown choices. One UI-flow audio-visualizer-source verification failed exit 101; only generic panic tail retained. Do not claim the UI flow works.
- Thumbnail helper now supplies deterministic audio to both generator/effect renderers. Exactly two generator thumbnails regenerated and hashes match JSON. Observed images are unexpectedly dark (faint blue trace on Oscilloscope, lower-right blue concentration on Spectrogram); do not treat them as visual acceptance.
- Thumbnail worker ran renderer clippy successfully and full nextest: 1978 passed, 1 failed: node_graph::freeze::markers::tests::fused_wgsl_snapshot_unchanged (markers.rs:441). Retained left/right diff includes removal of effect:Oscilloscope regions after its move to generators. Snapshot must be reviewed after final graph changes; do not blindly update. Earlier worker failure remains unattributed. No base comparison established pre-existing status.
- Retained full nextest XML copied out of cache to /tmp/audio-generator-nextest-junit.xml. It is diagnostic output, not required source.
- No landing gate run; no merge or app push. Two failed GPU attempts exhausted workstream check budget. BUG-zimp tracks continuation.
- Archive destination is origin https://github.com/pk-97/MANIFOLD---Rust.git, verified PUBLIC via GitHub API. Reviewed changes contain repo source, preset assets, generated catalog/thumbnail assets and this handoff; no user project/media or secrets included.
