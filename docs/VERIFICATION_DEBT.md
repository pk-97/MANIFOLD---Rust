# Verification Debt — the unverified-surface ledger

<!-- index: Live ledger of shipped-but-not-fully-verified surfaces. One entry per gap between the verification level a landing reached and its target; burned down or consciously carried every wave. Format and rules: DESIGN_DOC_STANDARD.md §10. -->

Why this exists: "unverified interactively" notes used to live in landing reports and
memory, where they decayed silently into "shipped" — the bugs Peter found in the app
on 2026-07-05 (automation lanes present but not visibly working, glb import behaving
hard-coded) were all previously *recorded* as unverified, and nothing acted on the
record. This file makes the debt durable, in-repo, and impossible to close by
forgetting.

Rules (normative home: `DESIGN_DOC_STANDARD.md` §10):

- Every landing appends one entry per gap between the level reached and the target
  level (L0–L4 ladder in §10).
- Every orchestration wave either burns entries down (verify → move to **Closed**
  with date and how) or consciously carries them — the landing report says so.
  Silence is not carrying.
- IDs are stable (`VD-NNN`), never renumbered — they are referenced from landing
  reports (committed under `docs/landings/`, per §8.10), BUG_BACKLOG `Escaped:`
  lines, and memory.

---

## Open

### VD-001 — Automation lanes P1–P4: runtime pointer→command editing path — L1 reached / L4 target
Landed 2026-07-04 @ `8b306de0`. The full Ableton-style automation timeline UI
shipped; the live editing path (pointer → command → lane redraw in the running app)
was never observed — flagged at landing, then decayed. Peter hit it in the app
2026-07-05 (lanes/buttons present, lanes not visibly working; exact repro untriaged).
Burn-down: running-app smoke check (L4 today; L3 once UI_AUTOMATION P1–P2 lands).

### VD-002 — Preset library + picker P0–P6: interactive GUI matrix — L2 reached / L3 target
Landed 2026-07-04/05 (last `4c860cad`). Drag-drop, search-clear, the management
matrix, and thumbnail display are physically unautomatable headless today.
Burn-down: blocked on UI_AUTOMATION P1–P2 for L3; interim = a Peter click-script
(L4) covering the four flows.

### VD-003 — glTF import: correctness beyond the development fixture — L1 reached / L2 target
Landed with the glTF wave (foundation @ `47c878d7` + follow-ups). Peter reports
in-app import behavior "hard-coded or buggy" (2026-07-05, exact repro not yet
triaged). Burn-down: held-out-input gate — the two untracked fixtures already in
`tests/fixtures/gltf/` (`lowe.glb`, `cc0__japanese_apricot_prunus_mume.glb`) are the
held-out set; import each headless, render to PNG, look. Triage findings go to
BUG_BACKLOG with `Escaped:` lines.

### VD-004 — Audio layer export mixdown — L1 reached / L2 target
`audio_mixdown.rs` offline mix is unverified on a real export (recorded in memory as
"unverified on real export" since it shipped). Burn-down: one real export of a
stem-bearing project; listen to / inspect the output file.

### VD-005 — UI_AUTOMATION P1 selector surface: dumps read, no scripted drive yet — L2 reached / L3 target
Landed 2026-07-05 @ `3294eb9d`. The extended dump (widget/name + `custom_surfaces`
enumeration) was read at landing and all four target categories confirmed present with
payload ids — but nothing yet *drives* the surface: resolving a selector and
synthesizing a gesture against it is P2. Burn-down: lands with P2 — the two proving
flows (`select-and-inspect.json`, `drag-clip.json`) exercise the resolver against this
dump, taking the surface to L3. Minor open coverage gap folded in here: the
`editor` scene dump surfaces zero *named* widgets (graph-editor chrome names don't
appear in the headless editor scene — the naming pass covered transport/layer-header,
which show in the `timeline` scene); grow naming coverage organically as flows need it,
per §3 ("coverage grows organically").

*(VD-001–004 seeded 2026-07-05 from the memory corpus plus Peter's in-app findings;
the full backfill pass over recent landings is still owed and will extend this
list.)*

## Closed

*(none yet)*
