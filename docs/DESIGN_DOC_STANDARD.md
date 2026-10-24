# Design Doc Standard

Design docs turn architectural decisions into concrete, ready-to-execute code specs so fast workers (haiku / sonnet / free models) can implement them without guessing. Git history holds the history—never clutter code comments or docs with chat logs, bead numbers, or compliance rituals.

1. `**Status:** SHIPPED | IN PROGRESS | APPROVED | PROPOSED · <date>`

2. `## 1. Audit` — concise `file:line` table of existing code to extend or replace.

3. `## 2. Decisions` — `D1..Dn` with rationale and rejected alternatives (`Rejected: X, because Y`).

4. `## 3. Design & Data Model` — exact struct/enum/shader signatures, formulas, crate/thread ownership, and invariants.

5. `## 4. Tasks` — disjoint file sets, exact `file:line` changes, and 1–2 scoped verification commands (`cargo nextest run -p <crate> <filter>`).

## Rules for Code & Docs

- **Code comments explain *why* the code works, not git blame:** Never write bead IDs (`BUG-xxxx`), dates, session transcripts, or "Peter said on 2026-10-03" in source code comments. Git commit history owns when and why a change happened.

- **Tests verify behavior and math, never source text:** Write fast (<2s) unit/kernel tests on small inputs ($8^3$–$16^3$ grids, 1–3 steps) and closed-form invariants. Never write tests that regex-scan `.rs` files, count lines, or run 5-minute preset soaks as unit tests.

- **No process theatre:** No read-back essays, confession fields, L0–L4 verification debt ledgers, escape analysis, or committed landing reports.