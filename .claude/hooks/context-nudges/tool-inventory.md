# Before writing a script or tool

The tool you are about to write usually exists. Find it first, and name it in your report: reused, extended, or why neither fits.

- `scripts/TOOLS.md` (`scripts/dev.py --help`) is the repo's tool inventory, one line per verb (gate, land, gpu-proofs, gpu-queue, flows, capture, graph-tool, project-tool, the RT probes, the CPU oracles, worktree, storage, the detector lab).
- `tools/audio_analysis/` holds hundreds of analysis scripts; each module docstring says what it does. Search by concept, not by the name you planned: `rg -l '<concept>' tools/ scripts/`, then read the docstrings it finds.
- One-offs belong in your scratchpad, not in `scripts/`. A script that stays in `scripts/` gets a verb in `dev.py` (scripts/test_dev.py is red until it does).
