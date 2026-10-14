# Workspace-wide run reminder

Tests are scoped by default: `cargo nextest run -p <touched crate> [filter]`. A `--workspace` sweep is justified only at a multi-crate landing or when the blast radius genuinely crosses crates — state why in one line. GPU changes need the GPU proofs instead: `scripts/dev.py gpu-proofs` (scoped to the branch diff; `--filter <test>` runs one; it takes the GPU lock and never uses nextest). Most of the time a full workspace test just wastes time and tokens.
