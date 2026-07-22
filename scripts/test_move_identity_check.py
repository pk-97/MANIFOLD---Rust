#!/usr/bin/env python3
"""Self-test for move_identity_check.py — the pure-move / dispatch-split gate.

Builds throwaway git repos in a temp dir and runs the checker end-to-end (it
consumes real `git diff --color-moved` output, so synthetic diffs can't prove
the exit codes). Covers:

  1. pure move            → exit 0, residue 0            (a relocated fn)
  2. smuggled edit        → exit 1, residue > 0          (move + one changed line)
  3. dispatch-split       → exit 0, scaffold > 0, res 0  (arms → sub-dispatcher)
  4. dropped arm          → exit 1, residue > 0          (arm deleted, not re-added)
  5. scaffold over cap    → exit 1                       (too much structural glue)
  6. multi-line use move  → exit 0, residue 0            (D-18: brace-list moves)
  7. smuggled use-block   → exit 1, residue > 0          (D-18: code hidden in a
                                                           `use { ... }` block)
  8. context-opened       → exit 0, residue 0            (D-20 i: opener/closer
     use-block edit                                      unchanged, inner list
                                                           line edited)
  9. D-11 preamble move   → exit 0, residue 0, scaffold>0 (byte-exact 2-line
                                                           preamble recomputed atop
                                                           a moved dispatch_<d> fn)
 10. D-11 deviated        → exit 1, residue > 0          (one token off the
     preamble                                            byte-exact form — any
                                                           deviation = residue)
 11. drifted preamble     → exit 0, residue 0            (D-20 iii: inspector.rs's
     removed                                             actual drifted original
                                                           deleted, canonical form
                                                           recomputed elsewhere)
 12. impl-wrapper move    → exit 0, residue 0            (D-15: a bare inherent-impl
                                                           wrapper relocated into a
                                                           submodule is ALLOW wiring)
 13. impl-wrapper body    → exit 1, residue > 0          (D-15: a body edit hiding
     edit                                                 inside the moved wrapper)
 14. out-of-sequence      → exit 1, residue > 0          (D-21: a lone `");"`
     `");"` removed                                       removed OUTSIDE the
                                                           drifted-preamble opener
                                                           →sequence chain is
                                                           still caught, proving
                                                           generics no longer
                                                           mask genuine deletions)
 15. drifted preamble,    → exit 0, residue 0            (S5b: a sibling block
     moved-flagged `);`                                    moves to a file that
                                                           contains an identical
                                                           `);`, so git flags the
                                                           drifted preamble's own
                                                           `);` as MOVED — the
                                                           tracker must still
                                                           advance through it so
                                                           the NEXT drifted line
                                                           doesn't fall to
                                                           residue; reproduces
                                                           fb59db17's residue-1)
 16. router collapses to  → exit 0, residue 0            (S6b: the last domain
     bare `unhandled()`                                    arm is extracted,
     tail                                                  leaving `match
                                                           action { _ =>
                                                           unhandled() }` with
                                                           no arms — it
                                                           collapses to a bare
                                                           `DispatchResult::
                                                           unhandled()` tail
                                                           expression, the
                                                           router's null
                                                           action; the ADDED
                                                           bare line must
                                                           classify as
                                                           scaffold, not
                                                           residue)
 17. test-mod            → exit 0, residue 0, wiring>0   (D7a: a flat test mod
     distribution                                         distributed into
                                                           renamed, feature-
                                                           gated per-module test
                                                           mods; the header
                                                           lines have no removed
                                                           counterpart to
                                                           move-pair against, so
                                                           the D7a class must
                                                           claim them as wiring)
 18. smuggled test-mod   → exit 1, residue > 0            (D7a: a `static` line
     header                                               wedged inside a
                                                           test-mod header is
                                                           NOT header-shaped and
                                                           must fall through to
                                                           residue — the class
                                                           is smuggle-proof)
 19. include_str depth   → exit 0, residue 0, pairs>0     (D6: a test fn's
     rewrite                                              relative include_str!
                                                           path grows its leading
                                                           `../` run when the mod
                                                           moves deeper — a
                                                           forced pure-move edit
                                                           the class pairs off)
 20. include_str         → exit 1, residue > 0            (D6: the same move but
     smuggled path tail                                   the path TAIL changes
                                                           too (gain -> HACKED) —
                                                           a real behavior change
                                                           the class must catch)
 21. inline mod -> decl   → exit 0, residue 0             (W3-D2: one inline
     conversion                                           `#[cfg] mod X { … }`
                                                           becomes `mod X;` + a
                                                           sibling file; git
                                                           self-move-pairs the
                                                           re-added cfg line, so
                                                           the class must arm
                                                           BEFORE is_moved or the
                                                           `-mod X {` opener is
                                                           false residue)
 22. inline mod ->        → exit 0, residue 0             (W3-D2 / W3-D1: the
     #[path] decl                                         same conversion with a
     conversion                                           `#[path = "…"]` decl,
                                                           P3-R's tests-out form)
 23. inline mod           → exit 1, residue > 0           (W3-D2: a body line
     conversion,                                          edited alongside the
     smuggled body edit                                   conversion is caught —
                                                           the class only waives
                                                           header wiring)
 24. use-block            → exit 0, residue 0             (W3-D3: one combined
     redistributed,                                       multi-line use list
     moved openers                                        redistributed across
                                                           sibling modules; the
                                                           identical `use …{`
                                                           openers are all git-
                                                           moved, so open_block
                                                           must arm before
                                                           is_moved)
 25. use-block            → exit 1, residue > 0           (W3-D3: a real
     redistributed,                                       statement smuggled
     smuggled statement                                   inside a moved-opener
                                                           block is caught)
 26. consecutive #[path]  → exit 0, residue 0             (W3-D4: a run of inline
     mods, context cfg                                    test mods → #[path]
                                                           decls; git keeps the
                                                           `#[cfg]` line as
                                                           CONTEXT, so a context
                                                           cfg must arm the
                                                           following signed
                                                           `mod X {` opener)
 27. consecutive #[path]  → exit 1, residue > 0           (W3-D4: a body edit
     mods, smuggled body                                  alongside the context-
     edit                                                 cfg conversion is
                                                           caught — header wiring
                                                           only)

Run: python3 scripts/test_move_identity_check.py   (exit 0 = all pass)
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

CHECKER = str(Path(__file__).resolve().parent / "move_identity_check.py")


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True,
                   capture_output=True, text=True)


def init_repo(repo: Path) -> None:
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "selftest@example.com")
    git(repo, "config", "user.name", "selftest")


def commit_tree(repo: Path, files: dict[str, str], msg: str) -> None:
    for rel, content in files.items():
        p = repo / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", msg)


def run_checker(repo: Path) -> tuple[int, str]:
    r = subprocess.run([sys.executable, CHECKER, "HEAD"], cwd=repo,
                       capture_output=True, text=True)
    return r.returncode, r.stdout


def field(out: str, name: str) -> int:
    m = re.search(rf"{name}: (\d+)", out)
    return int(m.group(1)) if m else -1


# ── Fixture bodies ──────────────────────────────────────────────────────────
# A ≥3-line block so git's move detector fires.
HELPER = (
    "fn helper(x: i32) -> i32 {\n"
    "    let y = x + 1;\n"
    "    let z = y * 2;\n"
    "    z + y\n"
    "}\n"
)
HELPER_EDITED = HELPER.replace("y * 2", "y * 3")

# A dispatch match with two ≥3-line arms and the sentinel.
ARM_BROWSER = (
    "        PanelAction::BrowserRename(a) => {\n"
    "            let k = mode_to_kind(a);\n"
    "            ui.close();\n"
    "            DispatchResult::handled()\n"
    "        }\n"
)
ARM_SCENE = (
    "        PanelAction::SceneAdd(a) => {\n"
    "            let n = build_node(a);\n"
    "            project.push(n);\n"
    "            DispatchResult::structural()\n"
    "        }\n"
)
INSPECTOR_BASE = (
    "pub fn dispatch_inspector(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    "    match action {\n"
    + ARM_BROWSER
    + ARM_SCENE
    + "        _ => DispatchResult::unhandled(),\n"
    "    }\n"
    "}\n"
)
# Router keeps its name; the browser arm moves to a sub-dispatcher.
INSPECTOR_ROUTER = (
    "pub fn dispatch_inspector(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    "    let r = browser::dispatch_browser(action, ctx);\n"
    "    if !r.unhandled { return r; }\n"
    "    match action {\n"
    + ARM_SCENE
    + "        _ => DispatchResult::unhandled(),\n"
    "    }\n"
    "}\n"
)
BROWSER_MODULE = (
    "pub fn dispatch_browser(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    "    match action {\n"
    + ARM_BROWSER
    + "        _ => DispatchResult::unhandled(),\n"
    "    }\n"
    "}\n"
)
# Same router, but the browser arm is DROPPED (not re-homed anywhere).
INSPECTOR_ROUTER_DROPPED = (
    "pub fn dispatch_inspector(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    "    match action {\n"
    + ARM_SCENE
    + "        _ => DispatchResult::unhandled(),\n"
    "    }\n"
    "}\n"
)

# D-18 fixtures: a multi-line `use { ... }` brace-list import that moves
# across a module wall alongside the code that needs it (case 6), and a real
# statement smuggled inside an otherwise-open use block (case 7).
#
# Every identifier below is globally unique across the base/after/sub bodies
# (no shared tokens, including the import path on the opener line) so git's
# `--color-moved` can never recognize a line as unchanged context or as a
# move elsewhere in the diff — every changed line is forced through the
# ALLOW/use-block classifier, which is exactly what this fixture proves.
DISPATCH_BASE = (
    "use crate::widgets::{\n"
    "    AlphaWidget,\n"
    "    BetaWidget,\n"
    "    GammaWidget,\n"
    "};\n"
    "\n"
    + HELPER
)
# helper() moves out to sub.rs; the import that stays behind is edited to
# drop the now-dead names and pick up an unrelated one.
DISPATCH_AFTER_MOVE = (
    "use crate::sprockets::{\n"
    "    DeltaSprocket,\n"
    "};\n"
)
SUB_AFTER_MOVE = (
    "// sub\n"
    "use super::widgets::{\n"
    "    EpsilonThing,\n"
    "    ZetaThing,\n"
    "};\n"
    "\n"
    + HELPER
)
# A real statement smuggled between a use block's opener and its closer.
SMUGGLED_USE_BLOCK = (
    "use crate::types::{\n"
    "    Alpha,\n"
    '    println!("smuggled");\n'
    "    Beta,\n"
    "};\n"
    "// placeholder\n"
)

# D-20(i) fixture: a multi-line `use { ... }` whose OPENER and CLOSER are
# both UNCHANGED (context) lines — only an inner list line is edited (one
# name removed, a different name added). Before the fix, block tracking never
# armed (the opener never appears as a +/- line), so the inner +/- lines fell
# to residue. `KeepGadget,` stays byte-identical in both versions so it
# remains a genuine context line inside the block, proving the tracker
# doesn't need every inner line touched to work.
CONTEXT_USE_BASE = (
    "use crate::gadgets::{\n"
    "    OmicronGadget,\n"
    "    KeepGadget,\n"
    "};\n"
    "\n"
    "fn keep_fn() {}\n"
)
CONTEXT_USE_AFTER = (
    "use crate::gadgets::{\n"
    "    RhoGadget,\n"
    "    KeepGadget,\n"
    "};\n"
    "\n"
    "fn keep_fn() {}\n"
)


# D-11 fixtures: the byte-exact 2-line preamble a split-out `dispatch_<d>` fn
# recomputes at its top (it can't inherit the outer fn's locals). Case 8 proves
# the canonical form is recognized as scaffold when a fn moves across a module
# wall and gains it; case 9 proves one deviated token (smuggle-proofing, D-18
# precedent) is NOT recognized — it must fall through to residue.
PARAMS_BODY = (
    "    let scaled = ctx.value * 2;\n"
    "    let offset = scaled + 1;\n"
    "    DispatchResult::from(offset)\n"
)
DISPATCH_PARAMS_BASE = (
    "pub fn dispatch_params(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    + PARAMS_BODY
    + "}\n"
)
PREAMBLE_CANONICAL = (
    "    let (effective_tab, effective_active_layer) = super::editor_dispatch_context"
    "(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), "
    "ctx.active_layer);\n"
    "    let active_layer = &effective_active_layer;\n"
)
# One token deviated from the byte-exact form: the trailing arg is a different
# field (`ctx.previous_layer` instead of `ctx.active_layer`).
PREAMBLE_DEVIATED = (
    "    let (effective_tab, effective_active_layer) = super::editor_dispatch_context"
    "(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), "
    "ctx.previous_layer);\n"
    "    let active_layer = &effective_active_layer;\n"
)
PARAMS_MODULE = (
    "pub fn dispatch_params(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    + PREAMBLE_CANONICAL
    + PARAMS_BODY
    + "}\n"
)
PARAMS_MODULE_DEVIATED = (
    "pub fn dispatch_params(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    + PREAMBLE_DEVIATED
    + PARAMS_BODY
    + "}\n"
)

# D-20(iii) fixture: the drifted preamble actually present in inspector.rs's
# `dispatch_inspector` (verified against the source, not invented) — an
# explicit `&*ctx.active_layer` reborrow, an explicit `&Option<LayerId>` type
# annotation on the second `let`, and the call split across multiple lines.
# Proves the drifted form's REMOVED lines (the `-` side, when the last
# preamble-using domain moves out and the drifted original is deleted with
# nothing left behind) are recognized as scaffold, not residue. The ADD side
# uses the CANONICAL form (already proven by case_preamble_scaffold above) —
# this fixture is specifically about the removal-side drifted entries.
PREAMBLE_DRIFTED_INSPECTOR = (
    "    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(\n"
    "        ctx.editor_target,\n"
    "        &*ctx.project,\n"
    "        ctx.ui.inspector.last_effect_tab(),\n"
    "        &*ctx.active_layer,\n"
    "    );\n"
    "    let active_layer: &Option<LayerId> = &effective_active_layer;\n"
)
DISPATCH_PARAMS_BASE_DRIFTED = (
    "pub fn dispatch_params(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    + PREAMBLE_DRIFTED_INSPECTOR
    + PARAMS_BODY
    + "}\n"
)


# D-15 fixtures (P-F2a, merged from origin/main): a bare inherent-impl wrapper
# (`impl Foo {` + closing brace) relocated into a submodule is ALLOW-class
# wiring — the wrapper line carries no behavior, only the methods do — but a
# body edit hiding inside that moved wrapper is still caught. Ported into this
# harness's commit_tree/CASES style during the P-F2a→lane merge (D-19).
IMPL_FN_A = (
    "    fn a(&self) -> u32 {\n"
    "        let x = 1;\n"
    "        let y = 2;\n"
    "        x + y\n"
    "    }\n"
)
IMPL_FN_B = (
    "    fn b(&self) -> u32 {\n"
    "        let p = 10;\n"
    "        let q = 20;\n"
    "        p + q\n"
    "    }\n"
)
IMPL_FN_B_EDITED = IMPL_FN_B.replace("let q = 20", "let q = 30")
IMPL_BASE = "struct Foo;\nimpl Foo {\n" + IMPL_FN_A + "\n" + IMPL_FN_B + "}\n"
IMPL_MOD_AFTER = "struct Foo;\n\nmod overlay;\n\nimpl Foo {\n" + IMPL_FN_A + "}\n"
IMPL_OVERLAY_AFTER = "use super::*;\n\nimpl Foo {\n" + IMPL_FN_B + "}\n"
IMPL_OVERLAY_AFTER_EDIT = "use super::*;\n\nimpl Foo {\n" + IMPL_FN_B_EDITED + "}\n"


# D-21 fixture: a lone `");"` deleted OUTSIDE the drifted-preamble opener→
# sequence chain — nothing before it in the diff matches
# DRIFTED_PREAMBLE_SEQUENCE[0], so the stateful matcher never arms on it.
# Proves the sequence rework didn't regress to the D-20 iii bug it fixed: a
# short generic line that happens to also appear in the drifted sequence
# (here, the call-closer `");"`) must still be caught as residue when it is
# a genuine, unrelated deletion — never silently masked as scaffold.
OUT_OF_SEQUENCE_CLOSE_PAREN_BASE = (
    "fn caller() {\n"
    "    do_thing(\n"
    "        alpha,\n"
    "        beta,\n"
    "    );\n"
    "    tail();\n"
    "}\n"
)
OUT_OF_SEQUENCE_CLOSE_PAREN_AFTER = (
    "fn caller() {\n"
    "    do_thing(\n"
    "        alpha,\n"
    "        beta,\n"
    "    tail();\n"
    "}\n"
)


# S5b fixture (see classify()'s "S5b fix" comments): reproduces the exact
# moved-flag/tracker-desync collision the fix addresses. `caller()` is a
# ≥3-line block that moves verbatim to sub.rs (so git detects it as MOVED),
# and it happens to contain a `);` line IDENTICAL to the drifted preamble's
# own `);` closer (DRIFTED_PREAMBLE_SEQUENCE[5]). Confirmed against real git
# output: with both the caller() move and the drifted-preamble removal in the
# same diff, git's `--color-moved` independently flags that drifted `);` line
# as moved (it content-matches caller()'s own `);`, added elsewhere), even
# though it's really the dead drifted preamble being deleted, not a move.
# This is the exact shape of fb59db17's residue-1 regression: pre-S5b-fix,
# the moved-flagged `);` would `continue` before the tracker ever consulted
# it, leaving drifted_idx one step behind so the NEXT drifted line — `let
# active_layer: &Option<LayerId> = ...` — no longer matched
# DRIFTED_PREAMBLE_SEQUENCE[drifted_idx] and fell to residue. Verified
# directly (outside this harness) that the pre-fix checker gives exit 1,
# residue 1, with exactly that line as the reported residue; the post-fix
# checker gives exit 0, residue 0 on the identical fixture.
MOVED_COLLISION_CALLER = (
    "fn caller() {\n"
    "    do_thing(\n"
    "        alpha,\n"
    "        beta,\n"
    "    );\n"
    "    tail();\n"
    "}\n"
)
MOVED_COLLISION_BASE = (
    MOVED_COLLISION_CALLER + "\n" + DISPATCH_PARAMS_BASE_DRIFTED + "// tail\n"
)


# S6b fixture: the terminal router-collapse shape from the real ruling —
# `dispatch_inspector` starts as INSPECTOR_ROUTER (browser arm already
# extracted, one `match action { SCENE arm; _ => unhandled() }` left), and its
# LAST remaining arm (scene) is extracted too. With no arms left, the match
# has nothing to dispatch on, so it collapses to a bare
# `DispatchResult::unhandled()` tail expression — no `_ =>`, no trailing
# comma/semicolon, because it's now the fn's tail expr, not a match arm.
# Proves: the removed `match action {` / SCENE arm (MOVED, verbatim into
# scene.rs) / sentinel arm / closing brace are scaffold as before, AND the
# newly-ADDED bare `DispatchResult::unhandled()` line — previously
# unclassified residue — is now recognized as scaffold too.
ROUTER_FULLY_COLLAPSED = (
    "pub fn dispatch_inspector(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    "    let r = browser::dispatch_browser(action, ctx);\n"
    "    if !r.unhandled { return r; }\n"
    "    let r = scene::dispatch_scene(action, ctx);\n"
    "    if !r.unhandled { return r; }\n"
    "    DispatchResult::unhandled()\n"
    "}\n"
)
SCENE_MODULE = (
    "pub fn dispatch_scene(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {\n"
    "    match action {\n"
    + ARM_SCENE
    + "        _ => DispatchResult::unhandled(),\n"
    "    }\n"
    "}\n"
)


# D7a fixtures (Wave 2 P2-G): distributing one flat `#[cfg(test)] mod tests`
# into per-module test mods. The added test mods are RENAMED and FEATURE-GATED
# so their header lines (`#[cfg(all(test, feature = "…"))]`, `mod <name> {`)
# have no identical removed counterpart and cannot be git-move-paired — they
# fall to residue unless the D7a class claims them. This is exactly the
# threshold-fragile shape the class exists for; a naive same-name/`#[cfg(test)]`
# distribution git already pairs on its own and would not exercise the class.
# The two test bodies are ≥3-line moves (git detects them), so only the header
# wiring is left for the classifier to prove.
GRAPH_TEST_MOD_FLAT = (
    "// graph\n"
    "#[cfg(test)]\n"
    "mod tests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn alpha_undo_restores() {\n"
    "        let a = 1;\n"
    "        let b = 2;\n"
    "        assert_eq!(a + b, 3);\n"
    "    }\n"
    "    #[test]\n"
    "    fn beta_undo_restores() {\n"
    "        let c = 10;\n"
    "        let d = 20;\n"
    "        assert_eq!(c + d, 30);\n"
    "    }\n"
    "}\n"
)
GRAPH_MOD_SKELETON = "// graph\nmod node_edit;\nmod groups;\n"
NODE_EDIT_TEST_MOD = (
    "// node_edit\n"
    '#[cfg(all(test, feature = "graph_tests"))]\n'
    "mod nodetests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn alpha_undo_restores() {\n"
    "        let a = 1;\n"
    "        let b = 2;\n"
    "        assert_eq!(a + b, 3);\n"
    "    }\n"
    "}\n"
)
GROUPS_TEST_MOD = (
    "// groups\n"
    '#[cfg(all(test, feature = "graph_tests"))]\n'
    "mod grouptests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn beta_undo_restores() {\n"
    "        let c = 10;\n"
    "        let d = 20;\n"
    "        assert_eq!(c + d, 30);\n"
    "    }\n"
    "}\n"
)
# Smuggle: a real statement wedged inside the test-mod header, between the
# `mod nodetests {` opener and the first test. The cfg attr + opener are wiring;
# the `static` line is NOT header-shaped and must fall through to residue.
NODE_EDIT_TEST_MOD_SMUGGLED = (
    "// node_edit\n"
    '#[cfg(all(test, feature = "graph_tests"))]\n'
    "mod nodetests {\n"
    "    static SMUGGLED: u32 = compute_evil();\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn alpha_undo_restores() {\n"
    "        let a = 1;\n"
    "        let b = 2;\n"
    "        assert_eq!(a + b, 3);\n"
    "    }\n"
    "}\n"
)


# D6 fixtures (Wave 3): a test fn carrying a relative `include_str!("../…")`
# moves DEEPER (a.rs -> sub/b.rs), so its leading `../` run must grow by the
# added nesting depth — the only content change a deeper test-mod relocation
# forces onto a moved line. The include_str line is padded on both sides by
# ≥3 identical lines so git move-detects those blocks, isolating the include_str
# line as the sole non-moved change; the D6 class must pair it (depth rewrite
# PROVEN). The smuggle case additionally alters the path TAIL (gain -> HACKED),
# which changes the loaded shader — a real behavior change the class must CATCH.
INCLUDE_STR_FN_SHALLOW = (
    "fn load_kernel() -> &'static str {\n"
    "    let a1 = 1;\n"
    "    let a2 = 2;\n"
    "    let a3 = 3;\n"
    '    let original = include_str!("../primitives/shaders/gain.wgsl");\n'
    "    let b1 = 4;\n"
    "    let b2 = 5;\n"
    "    let b3 = 6;\n"
    "    original\n"
    "}\n"
)
INCLUDE_STR_FN_DEEP = INCLUDE_STR_FN_SHALLOW.replace(
    '"../primitives', '"../../primitives'
)
INCLUDE_STR_FN_DEEP_SMUGGLED = INCLUDE_STR_FN_DEEP.replace(
    "gain.wgsl", "HACKED.wgsl"
)


# W3-D2 fixtures (Wave 3): converting ONE inline `#[cfg(...)] mod X { … }` into a
# `mod X;` declaration + sibling file. git's `--color-moved=plain` pairs the
# re-added identical cfg attribute as a self-move (plain mode has no minimum
# block size), which short-circuits D7a's arming unless pending_test_attr is
# armed BEFORE the is_moved check — otherwise the `-mod X {` opener has no `;`
# counterpart to move-pair against and falls to false residue. This is the exact
# shape of P3-C's range residue (`-mod dispatch_contract_tests {` /
# `-mod gpu_tests {`). Verified against the pre-fix verifier (033e87f0): this
# fixture gives residue 1 (`-mod inline_tests {`) before the arming fix, 0 after.
#
# The `#[cfg(test)]` line must genuinely RELOCATE for git to self-move-pair it
# (the whole point of the bug) — so the inline test mod sits at the BOTTOM of
# BASE (below a kept `fn keep`) and the decl is hoisted to the TOP in AFTER,
# mirroring the real P3-C where the decl joins the mod declarations while the
# inline block is removed from further down. The `fn keep` block stays common
# context and separates the old cfg location from the new one. Case A: plain
# `mod X;` conversion (P3-C tests.rs/gpu_tests). Case B: the `#[path = "…"]`
# tests-out form (W3-D1 / P3-R's 11 decls). Case C: a body edit smuggled
# alongside the conversion must still be caught.
INLINE_TEST_MOD_BASE = (
    "mod entry;\n"
    "mod other;\n"
    "\n"
    "fn keep() {\n"
    "    let x = 1;\n"
    "    let y = 2;\n"
    "    x + y\n"
    "}\n"
    "\n"
    "#[cfg(test)]\n"
    "mod inline_tests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn alpha_roundtrip() {\n"
    "        let a = 1;\n"
    "        let b = 2;\n"
    "        assert_eq!(a + b, 3);\n"
    "    }\n"
    "}\n"
)
# The module body as it lands in the sibling file (the file IS the module, so the
# contents are dedented one level; `--color-moved-ws=ignore-all-space` pairs the
# re-indented block as a move).
INLINE_TEST_MOD_BODY = (
    "use super::*;\n"
    "#[test]\n"
    "fn alpha_roundtrip() {\n"
    "    let a = 1;\n"
    "    let b = 2;\n"
    "    assert_eq!(a + b, 3);\n"
    "}\n"
)
# One body line edited alongside the conversion (let b = 2 -> 99): a real
# behavior change the class must CATCH.
INLINE_TEST_MOD_BODY_SMUGGLED = INLINE_TEST_MOD_BODY.replace("let b = 2", "let b = 99")
# After: the decl is hoisted above `fn keep` (so its cfg line relocates); the
# body moved to the sibling file.
INLINE_MOD_DECL_AFTER = (
    "mod entry;\n"
    "mod other;\n"
    "#[cfg(test)]\n"
    "mod inline_tests;\n"
    "\n"
    "fn keep() {\n"
    "    let x = 1;\n"
    "    let y = 2;\n"
    "    x + y\n"
    "}\n"
)
# The `#[path = "…"]` tests-out form (W3-D1 / P3-R): the decl gains a `#[path]`
# attribute and the sibling file lives under tests/.
INLINE_MOD_PATH_DECL_AFTER = (
    "mod entry;\n"
    "mod other;\n"
    "#[cfg(test)]\n"
    '#[path = "tests/inline_tests.rs"]\n'
    "mod inline_tests;\n"
    "\n"
    "fn keep() {\n"
    "    let x = 1;\n"
    "    let y = 2;\n"
    "    x + y\n"
    "}\n"
)


# W3-D3 fixtures (Wave 3, P3-G): a directory split redistributes ONE combined
# multi-line `use path::{ … }` import list across sibling modules. git's
# --color-moved=plain flags every `use path::{` OPENER as moved (identical text
# recurs on the removed 1× and added N× sides), so it short-circuits before the
# ALLOW branch arms open_block — the D-18 tracker never opens and the item
# continuation lines fall to residue UNLESS open_block is armed BEFORE is_moved.
# The items are RE-GROUPED across physical lines (Alpha,Beta,Gamma / Delta,…
# regrouped to Alpha,Beta / Gamma / …) so no removed line move-pairs a single
# added line — exactly P3-G's manifold_core::effect_graph_def redistribution.
# Function bodies are ≥3 lines so git move-detects them, isolating the imports.
# Case A: PROVEN residue 0. Case B: a real statement smuggled inside a moved-
# opener block is CAUGHT (USE_ITEM smuggle-proofing unchanged).
USEBLOCK_REGROUP_BASE = (
    "use foo::bar::{\n"
    "    Alpha, Beta, Gamma,\n"
    "    Delta, Epsilon, Zeta,\n"
    "};\n"
    "\n"
    "fn part_one() {\n"
    "    let _ = (Alpha, Beta, Gamma);\n"
    "    let p = 1;\n"
    "    let q = 2;\n"
    "    let r = 3;\n"
    "}\n"
    "\n"
    "fn part_two() {\n"
    "    let _ = (Delta, Epsilon, Zeta);\n"
    "    let s = 4;\n"
    "    let t = 5;\n"
    "    let u = 6;\n"
    "}\n"
)
USEBLOCK_REGROUP_ONE = (
    "use foo::bar::{\n"
    "    Alpha, Beta,\n"
    "    Gamma,\n"
    "};\n"
    "\n"
    "fn part_one() {\n"
    "    let _ = (Alpha, Beta, Gamma);\n"
    "    let p = 1;\n"
    "    let q = 2;\n"
    "    let r = 3;\n"
    "}\n"
)
USEBLOCK_REGROUP_TWO = (
    "use foo::bar::{\n"
    "    Delta, Epsilon,\n"
    "    Zeta,\n"
    "};\n"
    "\n"
    "fn part_two() {\n"
    "    let _ = (Delta, Epsilon, Zeta);\n"
    "    let s = 4;\n"
    "    let t = 5;\n"
    "    let u = 6;\n"
    "}\n"
)
# A real statement wedged inside the moved-opener block in one.rs: not
# USE_ITEM-shaped, must fall through to residue.
USEBLOCK_REGROUP_ONE_SMUGGLED = USEBLOCK_REGROUP_ONE.replace(
    "    Gamma,\n", "    Gamma,\n    let evil = compute();\n"
)


# W3-D4 fixtures (Wave 3, P3-R): a RUN of consecutive inline `#[cfg(test)] mod
# X_tests { … }` test mods converted to `#[cfg(test)] #[path="tests/X.rs"] mod
# X_tests;` decls + sibling files. git's minimal diff anchors the identical,
# unchanged `#[cfg(test)]` lines as CONTEXT (not signed self-moves) and diffs
# only the mod lines — so the signed-cfg arm (W3-D2) never fires and each
# `-mod X_tests {` opener falls to residue unless a CONTEXT cfg line also arms
# the following signed opener (verified against P3-R's real e09e078b: 11/11
# openers preceded by a context cfg; residue 11 pre-fix, 0 for these post-fix).
# Case A PROVEN residue 0; case B smuggles a body edit alongside the conversion
# → CAUGHT (the context arm waives only the `mod X {` header, never body bytes).
CTX_CFG_MODS_BASE = (
    "mod real_code;\n"
    "\n"
    "#[cfg(test)]\n"
    "mod alpha_tests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn a1() {\n"
    "        let x = 1;\n"
    "        assert_eq!(x, 1);\n"
    "    }\n"
    "}\n"
    "\n"
    "#[cfg(test)]\n"
    "mod beta_tests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn b1() {\n"
    "        let y = 2;\n"
    "        assert_eq!(y, 2);\n"
    "    }\n"
    "}\n"
)
CTX_CFG_MODS_DECL_AFTER = (
    "mod real_code;\n"
    "\n"
    "#[cfg(test)]\n"
    '#[path = "tests/alpha_tests.rs"]\n'
    "mod alpha_tests;\n"
    "\n"
    "#[cfg(test)]\n"
    '#[path = "tests/beta_tests.rs"]\n'
    "mod beta_tests;\n"
)
CTX_CFG_ALPHA_BODY = (
    "use super::*;\n"
    "#[test]\n"
    "fn a1() {\n"
    "    let x = 1;\n"
    "    assert_eq!(x, 1);\n"
    "}\n"
)
CTX_CFG_BETA_BODY = (
    "use super::*;\n"
    "#[test]\n"
    "fn b1() {\n"
    "    let y = 2;\n"
    "    assert_eq!(y, 2);\n"
    "}\n"
)
CTX_CFG_ALPHA_BODY_SMUGGLED = CTX_CFG_ALPHA_BODY.replace(
    "assert_eq!(x, 1)", "assert_eq!(x, 99)"
)


def case_pure_move(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"a.rs": HELPER + "// tail\n", "b.rs": "// b\n"}, "base")
    commit_tree(repo, {"a.rs": "// tail\n", "b.rs": "// b\n" + HELPER}, "move")
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_smuggled(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"a.rs": HELPER + "// tail\n", "b.rs": "// b\n"}, "base")
    commit_tree(repo, {"a.rs": "// tail\n", "b.rs": "// b\n" + HELPER_EDITED}, "edit")
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_dispatch_split(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"inspector.rs": INSPECTOR_BASE}, "base")
    commit_tree(repo, {"inspector.rs": INSPECTOR_ROUTER,
                       "dispatch/browser.rs": BROWSER_MODULE}, "split")
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "scaffold") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_dropped_arm(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"inspector.rs": INSPECTOR_BASE}, "base")
    commit_tree(repo, {"inspector.rs": INSPECTOR_ROUTER_DROPPED}, "drop")
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_over_cap(repo: Path) -> tuple[bool, str]:
    # 30 sub-dispatcher signatures added at once — all scaffold, over the cap.
    base = INSPECTOR_BASE
    extra = "".join(
        f"pub fn dispatch_x{i}(action: &PanelAction, ctx: &mut Ctx) -> DispatchResult {{\n"
        f"    match action {{\n"
        f"        _ => DispatchResult::unhandled(),\n"
        f"    }}\n"
        f"}}\n"
        for i in range(11)  # 11 * 3 scaffold-matching lines = 33 > cap 25
    )
    commit_tree(repo, {"inspector.rs": base}, "base")
    commit_tree(repo, {"inspector.rs": base, "extra.rs": extra}, "bulk-scaffold")
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "scaffold") > 25
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_multiline_use_move(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"dispatch.rs": DISPATCH_BASE, "sub.rs": "// sub\n"}, "base")
    commit_tree(
        repo,
        {"dispatch.rs": DISPATCH_AFTER_MOVE, "sub.rs": SUB_AFTER_MOVE},
        "move",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_smuggled_use_block(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"dispatch.rs": "// placeholder\n"}, "base")
    commit_tree(repo, {"dispatch.rs": SMUGGLED_USE_BLOCK}, "smuggle")
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_context_use_block(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"dispatch.rs": CONTEXT_USE_BASE}, "base")
    commit_tree(repo, {"dispatch.rs": CONTEXT_USE_AFTER}, "context-use-edit")
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_preamble_scaffold(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"inspector.rs": DISPATCH_PARAMS_BASE + "// tail\n"}, "base")
    commit_tree(
        repo,
        {"inspector.rs": "// tail\n", "params.rs": PARAMS_MODULE},
        "split-with-preamble",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "scaffold") >= 2
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_preamble_deviated(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"inspector.rs": DISPATCH_PARAMS_BASE + "// tail\n"}, "base")
    commit_tree(
        repo,
        {"inspector.rs": "// tail\n", "params.rs": PARAMS_MODULE_DEVIATED},
        "split-with-deviated-preamble",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_drifted_preamble_removed(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"inspector.rs": DISPATCH_PARAMS_BASE_DRIFTED + "// tail\n"}, "base")
    commit_tree(
        repo,
        {"inspector.rs": "// tail\n", "params.rs": PARAMS_MODULE},
        "split-with-drifted-preamble-removed",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_impl_wrapper_move(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"mod.rs": IMPL_BASE}, "base")
    commit_tree(
        repo,
        {"mod.rs": IMPL_MOD_AFTER, "overlay.rs": IMPL_OVERLAY_AFTER},
        "wrapper-move",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_impl_wrapper_body_edit(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"mod.rs": IMPL_BASE}, "base")
    commit_tree(
        repo,
        {"mod.rs": IMPL_MOD_AFTER, "overlay.rs": IMPL_OVERLAY_AFTER_EDIT},
        "wrapper-body-edit",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_out_of_sequence_close_paren(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"caller.rs": OUT_OF_SEQUENCE_CLOSE_PAREN_BASE}, "base")
    commit_tree(
        repo,
        {"caller.rs": OUT_OF_SEQUENCE_CLOSE_PAREN_AFTER},
        "drop-out-of-sequence-close-paren",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_drifted_preamble_moved_collision(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"inspector.rs": MOVED_COLLISION_BASE}, "base")
    commit_tree(
        repo,
        {"inspector.rs": "// tail\n", "sub.rs": MOVED_COLLISION_CALLER,
         "params.rs": PARAMS_MODULE},
        "split-with-moved-flagged-drifted-close-paren",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_router_collapse_bare_unhandled(repo: Path) -> tuple[bool, str]:
    commit_tree(
        repo,
        {"inspector.rs": INSPECTOR_ROUTER, "dispatch/browser.rs": BROWSER_MODULE},
        "base",
    )
    commit_tree(
        repo,
        {"inspector.rs": ROUTER_FULLY_COLLAPSED, "dispatch/browser.rs": BROWSER_MODULE,
         "dispatch/scene.rs": SCENE_MODULE},
        "collapse-to-bare-unhandled",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "scaffold") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_test_mod_distribution(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"graph.rs": GRAPH_TEST_MOD_FLAT}, "base")
    commit_tree(
        repo,
        {"graph.rs": GRAPH_MOD_SKELETON,
         "node_edit.rs": NODE_EDIT_TEST_MOD,
         "groups.rs": GROUPS_TEST_MOD},
        "distribute-tests",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "wiring") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_smuggled_test_mod_header(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"graph.rs": GRAPH_TEST_MOD_FLAT}, "base")
    commit_tree(
        repo,
        {"graph.rs": GRAPH_MOD_SKELETON,
         "node_edit.rs": NODE_EDIT_TEST_MOD_SMUGGLED,
         "groups.rs": GROUPS_TEST_MOD},
        "distribute-tests-with-smuggle",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_include_str_depth_rewrite(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"a.rs": INCLUDE_STR_FN_SHALLOW, "sub/b.rs": "// b\n"}, "base")
    commit_tree(
        repo,
        {"a.rs": "// a\n", "sub/b.rs": "// b\n" + INCLUDE_STR_FN_DEEP},
        "move-deeper-with-include-str-depth-rewrite",
    )
    code, out = run_checker(repo)
    ok = (
        code == 0
        and field(out, "residue") == 0
        and field(out, "include_str pairs") > 0
        and field(out, "moved lines") > 0
    )
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_include_str_smuggled_path(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"a.rs": INCLUDE_STR_FN_SHALLOW, "sub/b.rs": "// b\n"}, "base")
    commit_tree(
        repo,
        {"a.rs": "// a\n", "sub/b.rs": "// b\n" + INCLUDE_STR_FN_DEEP_SMUGGLED},
        "move-deeper-with-smuggled-path-tail",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_inline_mod_to_decl(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"mod.rs": INLINE_TEST_MOD_BASE}, "base")
    commit_tree(
        repo,
        {"mod.rs": INLINE_MOD_DECL_AFTER, "inline_tests.rs": INLINE_TEST_MOD_BODY},
        "convert-inline-mod-to-decl",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_inline_mod_to_path_decl(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"mod.rs": INLINE_TEST_MOD_BASE}, "base")
    commit_tree(
        repo,
        {"mod.rs": INLINE_MOD_PATH_DECL_AFTER,
         "tests/inline_tests.rs": INLINE_TEST_MOD_BODY},
        "convert-inline-mod-to-path-decl",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_inline_mod_conversion_smuggled(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"mod.rs": INLINE_TEST_MOD_BASE}, "base")
    commit_tree(
        repo,
        {"mod.rs": INLINE_MOD_DECL_AFTER,
         "inline_tests.rs": INLINE_TEST_MOD_BODY_SMUGGLED},
        "convert-inline-mod-with-smuggled-body-edit",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_useblock_moved_opener_regroup(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"src.rs": USEBLOCK_REGROUP_BASE}, "base")
    commit_tree(
        repo,
        {"src.rs": "// src\n", "one.rs": USEBLOCK_REGROUP_ONE,
         "two.rs": USEBLOCK_REGROUP_TWO},
        "redistribute-imports-across-modules",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_useblock_moved_opener_smuggled(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"src.rs": USEBLOCK_REGROUP_BASE}, "base")
    commit_tree(
        repo,
        {"src.rs": "// src\n", "one.rs": USEBLOCK_REGROUP_ONE_SMUGGLED,
         "two.rs": USEBLOCK_REGROUP_TWO},
        "redistribute-imports-with-smuggle",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_ctx_cfg_consecutive_path_mods(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"src.rs": CTX_CFG_MODS_BASE}, "base")
    commit_tree(
        repo,
        {"src.rs": CTX_CFG_MODS_DECL_AFTER,
         "tests/alpha_tests.rs": CTX_CFG_ALPHA_BODY,
         "tests/beta_tests.rs": CTX_CFG_BETA_BODY},
        "convert-consecutive-path-mods",
    )
    code, out = run_checker(repo)
    ok = code == 0 and field(out, "residue") == 0 and field(out, "moved lines") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_ctx_cfg_conversion_smuggled(repo: Path) -> tuple[bool, str]:
    commit_tree(repo, {"src.rs": CTX_CFG_MODS_BASE}, "base")
    commit_tree(
        repo,
        {"src.rs": CTX_CFG_MODS_DECL_AFTER,
         "tests/alpha_tests.rs": CTX_CFG_ALPHA_BODY_SMUGGLED,
         "tests/beta_tests.rs": CTX_CFG_BETA_BODY},
        "convert-consecutive-path-mods-with-smuggle",
    )
    code, out = run_checker(repo)
    ok = code == 1 and field(out, "residue") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


CASES = [
    ("pure move -> exit 0", case_pure_move),
    ("smuggled edit -> exit 1", case_smuggled),
    ("dispatch-split scaffold -> exit 0", case_dispatch_split),
    ("dropped arm -> exit 1", case_dropped_arm),
    ("scaffold over cap -> exit 1", case_over_cap),
    ("multi-line use move -> exit 0 [D-18]", case_multiline_use_move),
    ("smuggled use-block -> exit 1 [D-18]", case_smuggled_use_block),
    ("context-opened use-block edit -> exit 0 [D-20 i]", case_context_use_block),
    ("D-11 preamble move -> exit 0, scaffold [PROVEN]", case_preamble_scaffold),
    ("D-11 deviated preamble -> exit 1 [CAUGHT]", case_preamble_deviated),
    ("drifted preamble removed -> exit 0 [D-20 iii]", case_drifted_preamble_removed),
    ("impl-wrapper move -> exit 0 [D-15]", case_impl_wrapper_move),
    ("impl-wrapper body edit -> exit 1 [D-15]", case_impl_wrapper_body_edit),
    ("out-of-sequence \");\" removal -> exit 1 [D-21, CAUGHT]",
     case_out_of_sequence_close_paren),
    ("drifted preamble removed, moved-flagged \");\" -> exit 0 [S5b, PROVEN]",
     case_drifted_preamble_moved_collision),
    ("router collapses to bare unhandled() tail -> exit 0 [S6b, PROVEN]",
     case_router_collapse_bare_unhandled),
    ("test-mod distribution -> exit 0 [D7a, PROVEN]", case_test_mod_distribution),
    ("smuggled test-mod header -> exit 1 [D7a, CAUGHT]",
     case_smuggled_test_mod_header),
    ("include_str depth rewrite -> exit 0 [D6, PROVEN]",
     case_include_str_depth_rewrite),
    ("include_str smuggled path tail -> exit 1 [D6, CAUGHT]",
     case_include_str_smuggled_path),
    ("inline mod -> decl conversion -> exit 0 [W3-D2, PROVEN]",
     case_inline_mod_to_decl),
    ("inline mod -> #[path] decl conversion -> exit 0 [W3-D2, PROVEN]",
     case_inline_mod_to_path_decl),
    ("inline mod conversion, smuggled body edit -> exit 1 [W3-D2, CAUGHT]",
     case_inline_mod_conversion_smuggled),
    ("use-block redistributed, moved openers -> exit 0 [W3-D3, PROVEN]",
     case_useblock_moved_opener_regroup),
    ("use-block redistributed, smuggled statement -> exit 1 [W3-D3, CAUGHT]",
     case_useblock_moved_opener_smuggled),
    ("consecutive #[path] mods, context cfg -> exit 0 [W3-D4, PROVEN]",
     case_ctx_cfg_consecutive_path_mods),
    ("consecutive #[path] mods, smuggled body edit -> exit 1 [W3-D4, CAUGHT]",
     case_ctx_cfg_conversion_smuggled),
]


def main() -> int:
    failures = 0
    for name, fn in CASES:
        with tempfile.TemporaryDirectory() as td:
            repo = Path(td)
            init_repo(repo)
            try:
                ok, detail = fn(repo)
            except Exception as e:  # noqa: BLE001 — surface any fixture breakage
                ok, detail = False, f"EXCEPTION {e}"
        print(f"  [{'PASS' if ok else 'FAIL'}] {name}  ({detail})")
        failures += not ok
    if failures:
        print(f"move_identity_check self-test: {failures} FAILED")
        return 1
    print(f"move_identity_check self-test: all {len(CASES)} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
