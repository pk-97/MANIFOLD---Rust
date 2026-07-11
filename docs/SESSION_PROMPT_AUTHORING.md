# Session Prompt Authoring — how to brief a session before it starts

**Status: NORMATIVE working guide · 2026-07-06 · Fable.**
**Audience: the model cutting session prompts after Fable — Opus, briefing a fresh
session of itself (solo or orchestrating Sonnet workers).**

[DESIGN_AUTHORING.md](DESIGN_AUTHORING.md) is how to think when producing a design.
This is the downstream sibling: how to brief the session that executes one. A design
doc is the contract; the session prompt is the briefing that carries everything the
doc *cannot* carry — the git mode, the verification gates, the traps specific to this
work, and what Peter owes or is owed. Every prompt in the Fable-era corpus (the
`fable-window-handoff` memory's Prompts C–F, and the Opus pack that follows this
guide) was built to this shape; they are the model instances. When this guide and a
newer instance disagree, the instance won — figure out why and update the guide.

The test of a good prompt: a fresh session with zero conversation history executes
the work without inventing anything this guide or the design doc already decided.

---

## 1. The opening brief to Peter — mandatory, first thing, before any work

Every prompt instructs its session to open with a **workload brief** and then STOP
for Peter's go-ahead. This is not ceremony: Peter re-prioritizes between sessions,
and a session that starts executing a stale plan wastes its whole budget. The brief
is also where he catches a misread of scope while it's still free to fix — and, via
the property and forks lines below, a misread of *intent*: a plan faithful to the
steps while the abstract goal quietly drifts. That failure passes every gate
(compiles, tests, lands) because it's conformance to intent, not correctness — so
only Peter reading the brief catches it. BUG-061 is the specimen: the executor
understood "make reset the slider's own gesture", built one shared reset action, but
registered it per-panel "for lower risk" — leaving the exact skippable seam the goal
existed to close, which surfaced as a whole panel of sliders with no reset. Steps can
be right while the thing that had to be true isn't.

Format — short, plain language, no jargon the prompt didn't define:

```
## Session brief — <name>

**What this session does:** <2–3 sentences, instrument terms — what Peter can DO
after this lands that he can't do now.>

**The property this must hold:** <the abstract invariant the change has to satisfy,
one sentence — the end state, NOT the steps. "Reset is intrinsic to the slider: one
mechanism, no skippable seam", not "add a reset action to each panel". The steps below
are one route to it; this is the thing that must be true when they're done, and every
fork gets decided against THIS, not against blast radius.>

**The work, in order:**
1. <phase — one line each, plain words>
2. …

**Forks I'm about to decide:** <every choice the design leaves open that I'll resolve
while building — named, with the branch I lean to and why it serves the property above.
At an undecided fork, "safer" / "smaller blast radius" is usually "weaker against the
property" — surface the fork here rather than picking it quietly. "None" is a claim,
not a default: if I found none, I either read the design too shallowly to see them or
I'm about to bake them in silent.>

**What lands today:** <the concrete deliverables>
**What stays owed:** <anything deferred: Peter's feel-pass, a follow-up wave, a
blocked phase — named explicitly so nothing silently drops>
**Rough shape:** <expected duration/scale — e.g. "4 worker waves, most of a day"
or "solo, a few hours">

Anything you want to discuss or re-order before I start?
```

Then **wait for the answer**. If Peter says go, go — don't re-ask at each phase
(`feedback_dont_ask_to_stop`). If he redirects, the brief just paid for itself.

---

## 2. Anatomy — the eight blocks every prompt carries

Order matters less than presence. A prompt missing one of these is not done.

1. **Role line.** Session type (design / build / hardening / hunt / UX pass), model
   and effort, solo or orchestrating, and whether Peter is at the machine. "You are
   Opus (high) orchestrating Sonnet workers; Peter is reachable at forks" changes
   every downstream decision about escalation and verification.

2. **The opening brief instruction** (§1). Verbatim requirement, first action —
   including the property line (the invariant the change must hold) and the forks
   line (undecided choices surfaced, not picked quietly). A brief that lists only
   the steps is the BUG-061 hole.

3. **Read-first list.** The design doc WHOLE (never sections — the traps live in the
   parts that look skippable), its contract header, `DESIGN_DOC_STANDARD.md` §5–§6,
   the context docs the design names, and the **binding memories by exact name** so
   the session can pull them. Naming a memory is how judgment crosses the context
   boundary — "audio-stays-on-perform-surface" in the prompt is worth a paragraph of
   prose about why sends aren't graph nodes.

4. **Build order, with the standalone win first.** If one phase pays for itself even
   if the session dies after it (a live defect, a hot-path cost, a stale doc), it
   goes first and the prompt says why. Otherwise the doc's order stands.

5. **Traps — the judgment section.** 2–3 named hazards specific to THIS work. See §3
   for where to find them. **If you cannot name any, you have not read enough to cut
   the prompt.** Generic hazards (clippy, don't break main) don't count.

6. **Verification gates, matched to the work type.** See §4. The prompt states the
   gate per phase, not "test appropriately".

7. **Git mode, explicit.** Mode A or Mode B (§5) with the concrete names filled in:
   branch name, worktree path, which crates the focused gate runs. Never "use good
   git hygiene".

8. **Deliverables checklist.** What "done" means: code landed (or doc committed),
   the design doc's status line updated **same session** (DESIGN_DOC_STANDARD §8.9 —
   landings update their docs), the project memory updated, `BUG_BACKLOG.md` entries
   for anything found-not-fixed, and the feel-pass list if anything needs live human
   judgment.

---

## 3. Finding the traps — where the judgment lives

The trap section is the part that used to be Fable's contribution. It transfers by
knowing where trap knowledge is stored, not by intuition. Check all five, every time:

1. **The design doc's own negative gates** and "Decided — do not reopen" sections.
   The doc's author already foresaw the plausible-wrong turn; the prompt repeats it
   so the executor meets it before the temptation, not after.
2. **The binding memories.** Scan `MEMORY.md` for every entry touching the files,
   crates, or concepts in scope. The feedback memories are compiled corrections —
   each one is a trap somebody already fell into.
3. **`guide_common_mistakes` + `docs/BUG_BACKLOG.md`** filtered to the touched area.
   An open bug adjacent to the work is either in scope (say so) or a hazard to not
   trip (say that).
4. **The hot-path and thread-residency questions** (DESIGN_AUTHORING §1's binding
   constraints). If any phase touches per-frame code or crosses the two-thread
   boundary, the prompt says so and names the discipline that applies.
5. **The class of work itself.** Decomposition work → fuse-for-parity is the known
   failure (`migration-agents-bundle-instead-of-compose`). UI work → claiming visual
   wins from code reading (`grep-silence-isnt-absence-for-visual-elements`). Sync or
   invariant work → silently pinning something unverified: anything the maps have
   not ruled correct is marked **VERIFY-WITH-PETER** in the output, never asserted.

## 4. Verification gates by work type

The universal rule: verify one level closer to the stage than where you changed
things. Compiles ≠ correct ≠ looks right in the show.

| Work type | Gate the prompt must state |
|---|---|
| UI / visual | Headless PNG of every touched surface (`ui-headless-png-verification` memory); the **orchestrator** reads the PNGs and judges before accepting a worker's phase. A green test is not a look. |
| Runtime behavior / bugs | Reproduce with `println!` + logs BEFORE theorizing; verify the fix by driving the flow, not by re-reading the diff. |
| GPU / shaders / freeze compiler | The default sweep is GPU-free — run `--features gpu-proofs` deliberately (scope per CLAUDE.md "Testing scope"). |
| Preset / JSON edits | `check-presets` after every edit, and remember it is not runtime (`feedback_check_presets_is_not_runtime`). |
| Docs-only | Anchors verified against current code (evidence goes stale in days — Prompt C's queue entries were 3 days old and the repo had moved). |
| Anything feel-dependent | Not verifiable headless. LOG it for Peter's feel-pass with a one-line repro; never guess and never let a worker decide. |

Per phase: clippy on TOUCHED crates only (`cargo clippy -p <crate> -- -D warnings`)
and focused tests via `cargo nextest run -p <crate> --lib` (parallel test binaries;
GPU-proofs suites STAY on `cargo test` — the in-process `test_device` lock is the
device serializer, and nextest's process-per-test model would defeat it); batch the
phase's edits and verify ONCE at the end. The full workspace sweep (clippy + tests) runs ONCE per workstream, at
landing time, in the warm main checkout — never in a worktree, where it is a second
cold build (`feedback_prefer_focused_tests`, `.claude/GIT_TREE_DISCIPLINE.md` §2c).

## 5. Git modes — reference, don't restate

The spec is `.claude/GIT_TREE_DISCIPLINE.md` §2; the prompt names the mode and fills
in the blanks rather than re-deriving the rules.

- **Mode A — docs-only.** The worktree-guard hook (2026-07-08) denies agent edits
  in the main checkout, docs included — docs edits go through a worktree too (any
  idle one; no warm cargo target needed). Worktrees have a private index; in the
  MAIN checkout commit ONLY with explicit pathspec (`git commit -m '…' -- docs/…`;
  a NEW file gets one targeted `git add <path>` first). Land per §2: fetch → merge
  → `merge --no-ff` → push.
- **Mode B — code.** ONE warm worktree per workstream (never per phase), acquired
  via `python3 scripts/agent-worktree.py acquire <name> <branch> [--tip REF]` — it
  reuses the warmest idle pool dir, copies gitignored fixtures, and prints the
  step-0 line (still confirm the tip; a reused dir keeps its old name). NEVER the
  Agent tool's `isolation: "worktree"` (bases off the default branch); raw
  `git worktree add -b <branch> ".claude/worktrees/<name>" <verified-tip>` only if
  the script is unavailable. Every worker brief OPENS with the base check
  (`git -C "<worktree-abs>" log --oneline -1` matches the tip). Workers use
  `-C`/`--manifest-path` with absolute QUOTED paths (the repo path has a space) and
  NEVER land. The orchestrator lands from the main checkout — fetch → merge
  origin/main into the branch → gate → `merge --no-ff` → push → repeat if rejected
  — BATCHED per 2–3 phases (commits stay per-phase). No branch deletion until
  `git merge-base --is-ancestor` confirms; `agent-worktree.py release <name>` at
  session end.

## 6. Banned moves — in every prompt's blood, named when relevant

- Reading beyond the phase's read-back list. The design doc names what each phase
  reads; a worker sweeping the wider corpus burns tokens and wall-clock. Same for
  reports: raw data (anchors, hashes, verbatim gate output), never narrative.
- Pinning an invariant the maps haven't ruled correct. Mark VERIFY-WITH-PETER.
- Guessing at feel, motion timing, or taste. Log for the feel-pass.
- Claiming a visual outcome from code reading or grep silence. Render and look.
- Fusing dispatches "for parity" in decomposition work. Read DECOMPOSING_GENERATORS.
- Minimal-patching a structural bug (`feedback_fix_at_the_root_not_the_symptom`).
- Re-proposing anything in `guide_decision_log` or the don't-re-propose memories.
- `isolation: "worktree"`, bare `git add`/`commit` in the main checkout, `cd &&`.

## 7. Self-test — the prompt is done when

- [ ] A fresh session could run it with no conversation history and no follow-ups.
- [ ] It opens with the §1 brief-and-pause instruction — property line and forks
      line included, not just "the work, in order".
- [ ] All eight §2 blocks present; traps are specific, not generic.
- [ ] Every doc and memory it cites exists under that exact name (run the check —
      a mis-cited memory silently loads nothing).
- [ ] Verification gates are stated per phase and match §4.
- [ ] Git mode named with concrete branch/worktree/gate values.
- [ ] Deliverables include the §8.9 doc-status update and the memory update.
- [ ] Reading it, you can tell what Peter gets on stage — not just what the repo gets.
