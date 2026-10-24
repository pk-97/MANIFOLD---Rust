# Git Tree Discipline

1. **Work in the main checkout by default:** Edit, build, and test directly in the main working tree so incremental Cargo builds share the single warm `target/` directory. Never acquire or create git worktrees unless Peter explicitly asks for one.
2. **Parallel workers use disjoint file sets:** When running multiple worker subagents in parallel in the main checkout, assign each worker a strictly disjoint set of files and run one build/test pass after they finish.
3. **Commit with explicit file paths:** Always stage and commit with explicit paths (`git add -- <paths>`, `git commit -m '...' -- <paths>`), never `git add -A`, `git add .`, or `git commit -a`, so pre-existing untracked or dirty files stay untouched.
4. **Never force-push or rewrite `main`:** Land changes to `main` via clean commits or `--no-ff` merges after scoped `cargo nextest` and `cargo clippy` checks pass.
