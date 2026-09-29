# Audio route rejection — unverified work

Bead: BUG-kzgb. Branch: codex/audio-route-rejection.
Base: 0a3c1055fd5b9fdc9106bde1e8a80c3dd892b2f7.

Automatic approval review initially blocked retirement. The user subsequently
approved uploading a separate archive branch to the existing GitHub repository
and clearing slot-0 after verifying that backup, then reconfirmed after the
destination was clarified. This handoff is for archival recovery, not an app
landing; the final validation remains incomplete.

`AudioSetup::bind_send_to_layer` previously detached the layer from its current
send before discovering that the requested destination was absent. The command
ignored that failure and cleared redo history. The two changed routing modules
now preflight the destination and expose rejection through the existing command
hooks. Water/scene code is untouched.

The Luna worker reproduced the original core regression before fixing it, then
reported 20 core audio-setup unit tests and 9 editing audio-setup tests passing.
Its clippy run was interrupted; no passing clippy result is available.

The lead added service-level tests for preserved routing/history/data version,
failed-redo retry, local-snapshot then content execution, and recorded unbind.
The final editing test binary compiled successfully, but two launches produced
no test-harness output and stayed idle. Both were terminated with SIGTERM.
A one-second process sample showed only `_dyld_start` and a 96 KB footprint;
`codesign --verify --verbose=2` succeeded. This is not an assertion failure and
does not establish the final tests pass. Do not repeat launches without resolving
or gaining evidence about the startup stall. `git diff --check` passed.

Resume from slot-0, or from a verified archive if retirement is later approved.
Focused checks (replace SLOT_PATH):

```sh
cargo test --manifest-path 'SLOT_PATH/Cargo.toml' -p manifold-core --lib audio_setup::tests
cargo test --manifest-path 'SLOT_PATH/Cargo.toml' -p manifold-editing --lib commands::audio_setup::tests
cargo clippy --manifest-path 'SLOT_PATH/Cargo.toml' -p manifold-core -p manifold-editing --tests -- -D warnings
```

Review the final diff, then use `scripts/land_branch.py` for the required gate
and app landing. No landing gate was run and no app landing is claimed here.
The proposed archive contains only the two routing source files and this handoff beyond
the verified base. The destination is the existing origin repository,
`https://github.com/pk-97/MANIFOLD---Rust.git`. Visibility lookup was unavailable
(`gh` absent; unauthenticated GitHub API rate-limited). Treat the destination as
potentially public; no secrets, private fixtures, or logs are included.
