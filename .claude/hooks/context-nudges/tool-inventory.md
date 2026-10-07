# Before writing a script

`scripts/TOOLS.md` (`scripts/dev.py --help`) is the repo's tool inventory, one line per verb (gate, land, gpu-proofs, gpu-queue, flows, capture, graph-tool, project-tool, the RT probes, the CPU oracles, worktree, storage). Run it first: the tool you are about to write usually exists, and a one-off belongs in your scratchpad, not in `scripts/`. A script that stays in `scripts/` gets a verb in `dev.py` (scripts/test_dev.py is red until it does).
