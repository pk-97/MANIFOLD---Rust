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

Run: scripts/test_move_identity_check.py   (exit 0 = all pass)
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


def run_checker(repo: Path, *args: str) -> tuple[int, str]:
    r = subprocess.run([sys.executable, CHECKER, "HEAD", *args], cwd=repo,
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


def case_crate_skeleton(repo: Path, smuggle=False) -> tuple[bool, str]:
    commit_tree(repo, {"keep.rs": "// keep\n"}, "base")
    lib = ('//! A leaf crate.\n#![deny(unsafe_code)]\n'
           '#[cfg(feature = "testkit")]\npub mod testkit;\n'
           'use crate::engine::{\n    Alpha, Beta,\n};\n')
    if smuggle:
        lib += "fn smuggled() { perform(99); }\n"
    commit_tree(repo, {
        "crates/leaf/Cargo.toml": '[package]\nname = "leaf"\nversion = "0.1.0"\n',
        "crates/leaf/src/lib.rs": lib,
        "crates/leaf/src/main.rs": "mod engine;\n",
    }, "crate-skeleton")
    code, out = run_checker(repo)
    ok = code == int(smuggle) and field(out, "crate skeletons") > 0
    ok &= (field(out, "residue") > 0) == smuggle
    return ok, f"exit={code} {out.splitlines()[0]}"


MANIFEST_BASE = '[package]\nname = "app"\nversion = "0.1.0"\n'
MANIFEST_WIRING = (
    '[dependencies]\nleaf = { path = "../leaf" }\n'
    '[dev-dependencies]\nfixture = { package = "leaf", path = "../leaf" }\n'
    '[build-dependencies]\nbuilder = { package = "leaf", path = "../leaf" }\n'
    '[features]\nproofs = [\n    "leaf/gpu-proofs",\n    "fixture/testkit",\n]\n'
)


def case_manifest(repo: Path, mode="add") -> tuple[bool, str]:
    workspace = '[workspace]\nmembers = [\n    "crates/app",\n    "crates/leaf",\n]\n'
    commit_tree(repo, {
        "Cargo.toml": workspace,
        "crates/leaf/Cargo.toml": '[package]\nname = "leaf"\nversion = "0.1.0"\n',
        "crates/app/Cargo.toml": MANIFEST_BASE + (MANIFEST_WIRING if mode == "remove" else ""),
    }, "base")
    manifest = MANIFEST_BASE + MANIFEST_WIRING
    if mode == "version":
        manifest = manifest.replace('version = "0.1.0"', 'version = "0.2.0"')
    elif mode == "local-feature":
        manifest = manifest.replace('"leaf/gpu-proofs"', '"local-feature"')
    elif mode == "external-path":
        manifest = manifest.replace('path = "../leaf"', 'path = "../../external"')
    elif mode == "remove":
        manifest = MANIFEST_BASE
    elif mode == "default":
        manifest = manifest.replace("proofs =", "default =")
    elif mode in {"features", "default-features", "optional"}:
        option = {'features': '["testkit"]', 'default-features': 'false', 'optional': 'true'}[mode]
        manifest = manifest.replace('path = "../leaf" }', f'path = "../leaf", {mode} = {option} }}', 1)
    commit_tree(repo, {"crates/app/Cargo.toml": manifest}, "manifest-wiring")
    code, out = run_checker(repo)
    drift = mode not in {"add", "remove"}
    ok = code == int(drift) and (field(out, "residue") > 0) == drift
    if not drift:
        ok &= field(out, "manifest wiring") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_dependency_selector_removal(repo: Path, selector="features", gone=False,
                                    moved_table=False) -> tuple[bool, str]:
    option = {"features": '["testkit"]', "default-features": "false", "optional": "true"}[selector]
    base = MANIFEST_BASE + f'[dependencies]\nleaf = {{ path = "../leaf", {selector} = {option} }}\n'
    commit_tree(repo, {
        "Cargo.toml": '[workspace]\nmembers = ["crates/app", "crates/leaf"]\n',
        "crates/leaf/Cargo.toml": '[package]\nname = "leaf"\n',
        "crates/app/Cargo.toml": base,
    }, "base")
    after = MANIFEST_BASE if gone else MANIFEST_BASE + '[dependencies]\nleaf = { path = "../leaf" }\n'
    if moved_table:
        after = after.replace("[dependencies]", "[dev-dependencies]")
    commit_tree(repo, {"crates/app/Cargo.toml": after}, "remove-selector")
    code, out = run_checker(repo)
    return (code == int(not gone) and (field(out, "residue") > 0) == (not gone),
            f"exit={code} {out.splitlines()[0]}")


def case_manifest_carveout(repo: Path, kind="lints", new=False, near_miss=False) -> tuple[bool, str]:
    commit_tree(repo, {
        "Cargo.toml": '[workspace]\nmembers = ["crates/app", "crates/leaf"]\n',
        "crates/leaf/Cargo.toml": '[package]\nname = "leaf"\n',
        "crates/app/Cargo.toml": MANIFEST_BASE,
    }, "base")
    if kind == "lints":
        wiring = '[lints]\nworkspace = true\n'
        if near_miss:
            wiring += 'extra = true\n'
    elif kind == "empty-feature":
        wiring = '[features]\ntestkit = []\n'
    else:
        table = "dependencies" if near_miss else "dev-dependencies"
        if kind == "build-features":
            table = "build-dependencies"
        wiring = f'[{table}]\nleaf = {{ path = "../leaf", features = ["testkit"] }}\n'
    path = "crates/new/Cargo.toml" if new else "crates/app/Cargo.toml"
    commit_tree(repo, {path: MANIFEST_BASE + wiring}, "carveout")
    code, out = run_checker(repo)
    drift = near_miss or (kind == "empty-feature" and not new) or kind == "build-features"
    return (code == int(drift) and (field(out, "residue") > 0) == drift,
            f"exit={code} {out.splitlines()[0]}")


def case_path_rewrite(repo: Path, mode="exact") -> tuple[bool, str]:
    before = ('use crate::node_graph::Thing;\n'
              'fn invoke() {\n    crate::node_graph::run(42);\n'
              '    $crate::node_graph::emit!(value);\n}\n')
    after = before.replace("$crate::node_graph::", "manifold_node_engine::").replace(
        "crate::node_graph::", "manifold_node_engine::")
    if mode == "argument":
        after = after.replace("run(42)", "run(43)")
    if mode == "boundary":
        before = before.replace("crate::node_graph::run", "othercrate::node_graph::run")
        after = after.replace("manifold_node_engine::run", "othermanifold_node_engine::run")
    if mode in {"string-space", "spacing"}:
        before = before.replace("run(42)", 'run("a b")')
        after = after.replace("run(42)", 'run("ab")' if mode == "string-space" else 'run("a b")')
        if mode == "spacing":
            after = after.replace("    manifold_node_engine::run", "        manifold_node_engine::run")
    if mode == "moved-collision":
        before += "fn destination() {\n}\n"
        after += "fn destination() {\n    crate::node_graph::run(42);\n}\n"
    commit_tree(repo, {"caller.rs": before, "other.rs": "// other\n"}, "base")
    commit_tree(repo, {"caller.rs": after if mode != "cross-file" else "// caller\n",
                       "other.rs": after if mode == "cross-file" else "// other\n"}, "paths")
    args = [] if mode == "implicit" else ["--rewrite", "crate::node_graph::=manifold_node_engine::",
                                           "--rewrite", "$crate::node_graph::=manifold_node_engine::"]
    code, out = run_checker(repo, *args)
    drift = mode not in {"exact", "spacing"}
    ok = code == int(drift) and (field(out, "residue") > 0) == drift
    if mode == "exact":
        ok &= field(out, "path rewrites") == 6
    if mode in {"implicit", "cross-file"}:
        ok &= field(out, "path rewrites") == 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_new_manifest(repo: Path, mode="exact") -> tuple[bool, str]:
    commit_tree(repo, {
        "Cargo.toml": '[workspace]\nmembers = ["crates/app", "crates/leaf"]\n'
                      '[workspace.dependencies]\nserde = "1"\n',
        "crates/app/Cargo.toml": MANIFEST_BASE + '[dependencies]\nlog = "0.4"\n',
        "crates/leaf/Cargo.toml": '[package]\nname = "leaf"\n',
    }, "base")
    manifest = ('[package]\nname = "new"\nversion = "0.1.0"\nedition = "2024"\n'
                'publish = false\nlicense = "MIT"\ndescription = "A leaf"\n'
                '[dependencies]\nleaf = { path = "../leaf" }\n'
                'serde = { workspace = true }\nlog = "0.4"\n'
                '[features]\nproofs = ["leaf/gpu-proofs"]\n')
    if mode == "build":
        manifest = manifest.replace('publish = false', 'build = "build.rs"')
    elif mode == "default":
        manifest = manifest.replace("proofs =", "default =")
    elif mode == "external":
        manifest = manifest.replace('log = "0.4"', 'log = "0.5"')
    elif mode == "dep-features":
        manifest = manifest.replace('path = "../leaf"', 'path = "../leaf", features = ["testkit"]')
    elif mode == "table":
        manifest += '[profile.release]\nopt-level = 0\n'
    commit_tree(repo, {"crates/new/Cargo.toml": manifest}, "new-manifest")
    code, out = run_checker(repo)
    drift = mode != "exact"
    return (code == int(drift) and (field(out, "residue") > 0) == drift,
            f"exit={code} {out.splitlines()[0]}")


def case_bin_transfer(repo: Path, mode="exact") -> tuple[bool, str]:
    before = ('[[bin]]\nname = "inspect"\npath = "src/bin/inspect.rs"\n'
              'required-features = ["proofs"]\n')
    after = ('[[bin]]\nname = \'inspect\'\npath = \'src/bin/inspect.rs\'\n'
             'required-features = [\n    "proofs",\n]\n')
    if mode == "default":
        after = after.replace("path = 'src/bin/inspect.rs'\n", "")
    elif mode == "path":
        after = after.replace("src/bin/inspect.rs", "src/bin/other.rs")
    elif mode == "key":
        after += "test = false\n"
    elif mode == "features":
        after = after.replace('"proofs",', '"other",')
    elif mode == "name":
        after = after.replace("name = 'inspect'", "name = 'other'")
    elif mode == "old-key":
        before += "test = false\n"
    elif mode == "multiple":
        before += '[[bin]]\nname = "second"\n'
        after += "[[bin]]\nname = 'second'\n"
    elif mode == "duplicate":
        after += after
    commit_tree(repo, {
        "Cargo.toml": '[workspace]\nmembers = ["crates/app"]\n',
        "crates/app/Cargo.toml": MANIFEST_BASE + before,
        "crates/app/src/bin/inspect.rs": HELPER,
        **({"crates/app/src/bin/second.rs": HELPER_EDITED} if mode == "multiple" else {}),
    }, "base")
    (repo / "crates/app/src/bin/inspect.rs").unlink()
    if mode == "multiple":
        (repo / "crates/app/src/bin/second.rs").unlink()
    commit_tree(repo, {
        "crates/app/Cargo.toml": MANIFEST_BASE + (before if mode == "retained" else ""),
        "crates/new/Cargo.toml": MANIFEST_BASE.replace('"app"', '"new"') + after,
        "crates/new/src/bin/inspect.rs": HELPER,
        **({"crates/new/src/bin/second.rs": HELPER_EDITED} if mode == "multiple" else {}),
    }, "bin-transfer")
    code, out = run_checker(repo)
    drift = mode not in {"exact", "default", "multiple"}
    ok = code == int(drift) and (field(out, "residue") > 0) == drift
    if not drift:
        ok &= field(out, "crate skeletons") > 0 and field(out, "manifest wiring") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_bin_source_transfer(repo: Path, mode="different", existing=False) -> tuple[bool, str]:
    declaration = '[[bin]]\nname = "inspect"\npath = "tools/inspect.rs"\n'
    old_source = "crates/app/tools/inspect.rs"
    new_source = "crates/new/tools/inspect.rs"
    destination = MANIFEST_BASE.replace('"app"', '"new"')
    source = HELPER + '\nfn caller() {\n    crate::graph::run(42);\n}\n'
    files = {
        "Cargo.toml": '[workspace]\nmembers = ["crates/app", "crates/new"]\n',
        "crates/app/Cargo.toml": MANIFEST_BASE + declaration,
    }
    if existing:
        files["crates/new/Cargo.toml"] = destination
    if mode != "missing":
        files[old_source] = "" if mode == "empty" else source
    if mode in {"different", "identical", "line-endings", "empty"}:
        files[new_source] = {
            "different": source.replace("run(42)", "run(99)"),
            "identical": source,
            "line-endings": source.replace("\n", "\r\n"),
            "empty": "",
        }[mode]
    commit_tree(repo, files, "base")
    moved = mode in {"rename", "rename-rewrite"}
    files = {
        "crates/app/Cargo.toml": MANIFEST_BASE,
        "crates/new/Cargo.toml": destination + declaration,
    }
    if moved:
        (repo / old_source).unlink()
        files[new_source] = (source.replace("crate::graph::", "new_graph::")
                             if mode == "rename-rewrite" else source)
    commit_tree(repo, files, "bin-source-transfer")
    # Prove the fixture exercises the rename branch, including a non-identical source.
    renames = subprocess.check_output(
        ["git", "diff", "--name-status", "-M", "HEAD^", "HEAD"], cwd=repo, text=True)
    args = ["--rewrite", "crate::graph::=new_graph::"] if mode == "rename-rewrite" else []
    code, out = run_checker(repo, *args)
    drift = mode in {"different", "missing", "line-endings"}
    ok = code == int(drift) and (field(out, "residue") > 0) == drift
    if moved:
        ok &= bool(re.search(rf"^R\d+\t{re.escape(old_source)}\t{re.escape(new_source)}$",
                             renames, re.MULTILINE))
    if not drift:
        ok &= field(out, "manifest wiring") > 0
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_external_dependency(repo: Path, mode="table", table="dependencies") -> tuple[bool, str]:
    precedent = ('[dependencies.codec]\nversion = "1.2"\n'
                 'features = ["read", "write"]\ndefault-features = false\n')
    if mode == "inline":
        dependency = (f'[{table}]\ncodec = {{ default-features=false, '
                      'features=["read", "write"], version="1.2" }\n')
    else:
        dependency = (f'[{table}.codec]\nfeatures = [\n    "read",\n    "write",\n]\n'
                      "version = '1.2'\ndefault-features = false\n")
    if mode == "version":
        dependency = dependency.replace("'1.2'", "'1.3'")
    elif mode == "features":
        dependency = dependency.replace('"write",', '"extra",')
    elif mode == "name":
        dependency = dependency.replace(".codec]", ".other]")
    elif mode == "workspace-root":
        precedent = precedent.replace("[dependencies.codec]", "[workspace.dependencies.codec]")
    elif mode == "inline-precedent":
        precedent = ('[dependencies]\ncodec = { version = "1.2", '
                     'features = ["read", "write"], default-features = false }\n')
    root = '[workspace]\nmembers = ["crates/app"]\n'
    commit_tree(repo, {
        "Cargo.toml": root + (precedent if mode == "workspace-root" else ""),
        "crates/app/Cargo.toml": MANIFEST_BASE + ("" if mode == "workspace-root" else precedent),
        "outside/Cargo.toml": MANIFEST_BASE + precedent,
    }, "base")
    if mode == "nonmember":
        # Only the outside manifest has this precedent at the old revision.
        commit_tree(repo, {"crates/app/Cargo.toml": MANIFEST_BASE}, "remove-precedent")
    commit_tree(repo, {"crates/new/Cargo.toml": MANIFEST_BASE.replace('"app"', '"new"') + dependency},
                "external-dependency")
    code, out = run_checker(repo)
    drift = mode in {"version", "features", "name", "nonmember"}
    return (code == int(drift) and (field(out, "residue") > 0) == drift
            and field(out, "crate skeletons") > 0,
            f"exit={code} {out.splitlines()[0]}")


def case_members(repo: Path, mode="add") -> tuple[bool, str]:
    workspace = '[workspace]\nmembers = [\n    "crates/app",\n]\n'
    leaf = '[package]\nname = "leaf"\n'
    removing = mode.startswith("remove")
    commit_tree(repo, {"Cargo.toml": workspace.replace('    "crates/app",',
                      '    "crates/app",\n    "crates/leaf",') if removing else workspace,
                      "crates/app/Cargo.toml": MANIFEST_BASE,
                      "crates/leaf/Cargo.toml": leaf}, "base")
    if mode in {"remove", "remove-leftover"}:
        (repo / "crates/leaf/Cargo.toml").unlink()
        if mode == "remove-leftover":
            (repo / "crates/leaf/leftover.txt").write_text("still here\n")
        else:
            # These moved manifest bytes should not create unrelated residue.
            (repo / "crates/archive").mkdir()
            (repo / "crates/archive/Cargo.toml").write_text(leaf)
    new_workspace = workspace if removing else workspace.replace(
        '    "crates/app",', '    "crates/app",\n    "crates/leaf",')
    if mode == "missing":
        new_workspace = new_workspace.replace("crates/leaf", "crates/missing")
    commit_tree(repo, {"Cargo.toml": new_workspace}, "members")
    code, out = run_checker(repo)
    drift = mode not in {"add", "remove"}
    return (code == int(drift) and (field(out, "residue") > 0) == drift,
            f"exit={code} {out.splitlines()[0]}")


def case_deleted_added_rewrite(repo: Path, smuggle=False) -> tuple[bool, str]:
    old = "crates/old/src/worker.rs"
    new = "crates/new/src/worker.rs"
    before = "fn work() {\n" + "".join(f"    crate::graph::run({i});\n" for i in range(8)) + "}\n"
    after = before.replace("crate::graph::", "new_graph::")
    if smuggle:
        after = after.replace("run(3)", "run(99)")
    commit_tree(repo, {old: before}, "base")
    (repo / old).unlink()
    commit_tree(repo, {new: after, "crates/new/Cargo.toml": '[package]\nname = "new"\n'}, "move-and-rewrite")
    code, out = run_checker(repo, "--rewrite", "crate::graph::=new_graph::")
    ok = code == 1 and field(out, "residue") > 0 and field(out, "path rewrites") == 0
    ok &= old in out and new in out and "land the git mv and the rewrite as separate commits" in out
    return ok, f"exit={code} {out.splitlines()[0]}"


def case_renamed_rewrite(repo: Path, smuggle=False) -> tuple[bool, str]:
    before = HELPER + '\nfn caller() {\n    crate::graph::run(42);\n}\n'
    after = before.replace("crate::graph::", "new_graph::")
    if smuggle:
        after = after.replace("run(42)", "run(43)")
    commit_tree(repo, {"old/worker.rs": before}, "base")
    (repo / "old/worker.rs").unlink()
    commit_tree(repo, {"new/worker.rs": after}, "rename-with-rewrite")
    code, out = run_checker(repo, "--rewrite", "crate::graph::=new_graph::")
    return (code == int(smuggle) and (field(out, "residue") > 0) == smuggle
            and field(out, "path rewrites") == (0 if smuggle else 2),
            f"exit={code} {out.splitlines()[0]}")


def case_context_move(repo: Path, kind='path', bad=False) -> tuple[bool, str]:
    old = 'crates/old/src/node_graph/worker.rs'
    new = 'crates/new/src/deep/exec/worker.rs'
    snippets = {
        'path': ('fn run(x: crate::node_graph::Thing) { super::accept(x, 42); }\n',
                 'fn run(x: crate::deep::exec::Thing) { crate::deep::exec::accept(x, 42); }\n'),
        'terminal': ('use crate::node_graph::worker;\n', 'use crate::deep::exec::worker;\n'),
        'impl': ('impl crate::node_graph::Trait for Worker {\n}\n',
                 'impl crate::deep::exec::Trait for Worker {\n}\n'),
        'inline': ('mod tests {\nfn test() { super::super::accept(42); }\n}\n',
                   'mod tests {\nfn test() { crate::deep::exec::accept(42); }\n}\n'),
        'glob-inline': ('mod tests { mod inner { use super::*; } }\n',
                        'mod tests { mod inner { use crate::deep::exec::worker::tests::*; } }\n'),
        'macro': ('primitive! { ty: $crate::node_graph::Thing, value: 42 }\n',
                  'primitive! { ty: $crate::deep::exec::Thing, value: 42 }\n'),
        'macro-local': ('primitive! { ty: $crate::node_graph::Thing, value: 42 }\n',
                        'primitive! { ty: $crate::deep::exec::Thing, value: 42 }\n'),
        'macro-call': ('old::primitive! { value: 42 }\n', 'new::primitive! { value: 42 }\n'),
        'macro-rooted': ('crate::primitive! { ty: crate::node_graph::Thing, value: 42 }\n',
                         'new::primitive! { ty: crate::deep::exec::Thing, value: 42 }\n'),
        'macro-chain': ('old::primitive! { value: 42 }\n', 'new::primitive! { value: 42 }\n'),
        'hygiene': ('primitive! { ty: $crate::node_graph::Thing, value: 42 }\n',
                    'primitive! { ty: $crate::deep::exec::Thing, value: 42 }\n'),
        'include-str': ('const S: &str = include_str!("../../shaders/a.wgsl");\n',
                        'const S: &str = include_str!("../../../shaders/a.wgsl");\n'),
        'include-bytes': ('const S: &[u8] = include_bytes!("../../shaders/a.wgsl");\n',
                          'const S: &[u8] = include_bytes!("../../../shaders/a.wgsl");\n'),
        'documented-include': ('/// Example: include_str!("missing.wgsl")\nfn value() { accept(42); }\n',
                              '/// Example: include_str!("missing.wgsl")\nfn value() { accept(42); }\n'),
        'literal': ('fn run() { crate::node_graph::accept("a b"); }\n',
                    'fn run() { crate::deep::exec::accept("a b"); }\n'),
        'continued-string': ('const S: &str = "one\\\ntwo";\nfn run() { crate::node_graph::accept(42); }\n',
                             'const S: &str = "one\\\ntwo";\nfn run() { crate::deep::exec::accept(42); }\n'),
        'opaque': ('/* " /* nested */ " */\nfn run() { crate::node_graph::accept(r#"crate::node_graph::Thing"#); }\n',
                   '/* " /* nested */ " */\nfn run() { crate::deep::exec::accept(r#"crate::node_graph::Thing"#); }\n'),
        'build': ('', ''),
        'build-raw': ('', ''),
    }
    before, after = snippets[kind]
    if bad:
        if kind == 'glob-inline':
            after = after.replace('::tests::*', '::different::*')
        elif kind in ('terminal', 'impl'):
            after = after.replace('::worker', '::wrong').replace('::Trait', '::Wrong')
        elif kind.startswith('include-'):
            after = after.replace('../../../', '../../../../')
        elif kind == 'hygiene':
            after = after.replace('$crate', 'crate')
        elif kind == 'literal':
            after = after.replace('a b', 'a  b')
        elif kind == 'opaque':
            after = after.replace('r#"crate::node_graph::Thing"#', 'r#"crate::deep::exec::Thing"#')
        elif kind == 'macro-chain':
            after = after.replace('new::primitive', 'evil::primitive')
        else:
            after = after.replace('42', '99')
    manifest = '[package]\nname = "old"\nversion = "0.1.0"\n'
    files = {'Cargo.toml': '[workspace]\nmembers = ["crates/*"]\n',
             'crates/old/Cargo.toml': manifest, old: HELPER + before,
             'crates/old/shaders/a.wgsl': '// unique shader\n' * 10}
    if kind in ('build', 'build-raw'):
        files['crates/old/build.rs'] = 'fn main() { native_source_identity::emit_source_identity(&root, &[\n    "src/node_graph/worker.rs",\n], "IDENTITY"); }\n'
        if kind == 'build-raw':
            files['crates/old/build.rs'] = 'const TEXT: &str = r#"\n    "src/node_graph/worker.rs",\n"#;\n'
    commit_tree(repo, files, 'base')
    (repo / old).unlink()
    (repo / 'crates/old/shaders/a.wgsl').unlink()
    files = {new: HELPER + after, 'crates/new/Cargo.toml': manifest.replace('"old"', '"new"'),
             'crates/new/shaders/a.wgsl': '// unique shader\n' * 10}
    if kind == 'build':
        files['crates/old/build.rs'] = ('fn main() { native_source_identity::emit_source_identity(&root, &[\n'
            '    "../new/src/deep/exec/' + ('wrong' if bad else 'worker') + '.rs",\n], "IDENTITY"); }\n')
    elif kind == 'build-raw' and bad:
        files['crates/old/build.rs'] = 'const TEXT: &str = r#"\n    "../new/src/deep/exec/worker.rs",\n"#;\n'
    commit_tree(repo, files, 'move')
    args = ['--rewrite', 'old::node_graph::=new::deep::exec::']
    if kind == 'macro-local':
        args = ['--rewrite', '$crate::node_graph::=$crate::deep::exec::']
    if kind in ('macro', 'macro-local', 'hygiene'):
        args += ['--rewrite', 'primitive!::=primitive!::']
    if kind in ('macro-call', 'macro-rooted'):
        args += ['--rewrite', 'old::primitive!::=new::primitive!::']
    if kind == 'macro-chain':
        args += ['--rewrite', 'old::=new::', '--rewrite', 'new::primitive!::=evil::primitive!::']
    code, out = run_checker(repo, *args)
    return code == int(bad), f'exit={code} {out.splitlines()[0] if out else "no output"}'


def case_move_metadata(repo: Path, kind='target', bad=False) -> tuple[bool, str]:
    old = 'crates/old/src/worker.rs'
    new = 'crates/new/src/worker.rs'
    package = '[package]\nname = "old"\nversion = "0.1.0"\n'
    wiring = ('[dev-dependencies]\ncodec = "1"\n'
              '[target.\'cfg(unix)\'.dev-dependencies]\nplatform = "2"\n'
              '[features]\nproof = []\nperf = ["proof"]\n')
    lock = ('version = 4\n\n[[package]]\nname = "old"\nversion = "0.1.0"\n'
            'dependencies = ["codec", "platform"]\n')
    commit_tree(repo, {'Cargo.toml': '[workspace]\nmembers = ["crates/*"]\n',
                       'Cargo.lock': lock, 'crates/old/Cargo.toml': package + (
                           wiring.replace("[target.'cfg(unix)'.dev-dependencies]", "[target.'cfg(unix)'.dependencies]")
                           if kind == 'target-from-normal' else wiring),
                       'crates/unrelated/Cargo.toml': '[package]\nname = "unrelated"\n[dependencies]\nevil = "9"\n',
                       old: HELPER}, 'base')
    (repo / old).unlink()
    manifest = package.replace('"old"', '"new"') + wiring
    if bad and kind == 'target':
        manifest = manifest.replace('cfg(unix)', 'cfg(windows)')
    elif bad and kind == 'target-from-normal':
        manifest = manifest.replace('platform = "2"', 'platform = "3"')
    elif bad and kind == 'feature':
        manifest = manifest.replace('perf = ["proof"]', 'perf = []')
    elif bad and kind == 'dependency':
        manifest = manifest.replace('codec = "1"', 'evil = "9"')
    files = {'crates/new/Cargo.toml': manifest, new: HELPER}
    if kind == 'forward':
        files['crates/old/Cargo.toml'] = (package + '[dependencies]\nnew = { path = "../new" }\n' +
            wiring.replace('perf = ["proof"]', 'perf = ["new/perf", "' + ('wrong' if bad else 'proof') + '"]'))
    if kind in ('lock', 'lock-duplicate'):
        files['Cargo.lock'] = lock + ('\n[[package]]\nname = "new"\nversion = "0.1.0"\n'
                                     'dependencies = ["codec", "' + ('evil' if bad and kind == 'lock' else 'platform') + '"]\n')
        if kind == 'lock-duplicate' and bad:
            files['Cargo.lock'] += '\n[[package]]\nname = "new"\nversion = "9.0"\ndependencies = ["evil"]\n'
    commit_tree(repo, files, 'move')
    code, out = run_checker(repo)
    return code == int(bad), f'exit={code} {out.splitlines()[0] if out else "no output"}'


def case_import_identity(repo: Path, mode='split') -> tuple[bool, str]:
    before = 'use old::group::{A, B};\n'
    after = 'use new::group::A;\nuse new::group::B;\n'
    maps = ['old::group::=new::group::']
    positive = {'split', 'merge', 'reorder', 'nested', 'attributes', 'mixed', 'inline', 'raw-absolute'}
    if mode == 'split':
        after = 'use left::A;\nuse right::B;\n'
        maps = ['old::group::A::=left::A::', 'old::group::B::=right::B::']
    elif mode == 'merge':
        before = 'use old::group::A;\nuse old::group::B;\n'
        after = 'use new::group::{A, B};\n'
    elif mode == 'reorder':
        after = 'use new::group::{B, A};\n'
    elif mode == 'nested':
        before = 'pub use old::{group::{self as Group, A as Alias, nested::{X, Y}}, other::*};\n'
        after = ('pub use new::group::nested::{Y, X};\n'
                 'pub use new::group::{self as Group, A as Alias};\npub use new::other::*;\n')
        maps = ['old::=new::']
    elif mode == 'raw-absolute':
        before = 'use ::old::group::{r#type as r#match, B as _};\n'
        after = 'use ::new::group::B as _;\nuse ::new::group::r#type as r#match;\n'
    elif mode in {'attributes', 'attribute-change'}:
        attrs = '#[cfg(feature = "proof")]\n#[allow(unused_imports)]\n'
        before = attrs + 'pub(crate) ' + before
        after = attrs + 'pub(crate) use new::group::B;\n' + attrs + 'pub(crate) use new::group::A;\n'
        if mode == 'attribute-change':
            after = after.replace('unused_imports', 'dead_code', 1)
    elif mode == 'drop':
        after = 'use new::group::A;\n'
    elif mode == 'add':
        after += 'use new::group::C;\n'
    elif mode == 'alias':
        before = 'use old::group::{A as Alias, B};\n'
        after = 'use new::group::A as Different;\nuse new::group::B;\n'
    elif mode == 'visibility':
        before = 'pub(crate) ' + before
        after = 'pub use new::group::{A, B};\n'
    elif mode == 'duplicate-visibility':
        before = 'pub ' + before
        after = 'pub pub use new::group::{A, B};\n'
    elif mode == 'glob':
        after = 'use new::group::*;\n'
    elif mode == 'cfg-between-leaves':
        before = '#[cfg(a)]\nuse old::group::A;\n#[cfg(b)]\nuse old::group::B;\n'
        after = '#[cfg(b)]\nuse new::group::A;\n#[cfg(a)]\nuse new::group::B;\n'
    elif mode == 'wrong-symbol':
        after = 'use new::group::{A, Different};\n'
    elif mode == 'duplicate':
        after = 'use new::group::{A, B, B};\n'
    elif mode in {'mixed', 'mixed-body'}:
        before = before.rstrip() + ' const VALUE: u32 = 42;\n'
        after = after.replace('\n', ' ') + 'const VALUE: u32 = ' + ('42' if mode == 'mixed' else '99') + ';\n'
    elif mode == 'malformed-body':
        after = 'use new::group::{A, fn smuggled() { perform(99); }, B};\n'
    elif mode == 'scope':
        before = 'fn first() {\n' + before + '}\nfn second() {\n}\n'
        after = 'fn first() {\n}\nfn second() {\n' + after + '}\n'
    elif mode == 'inline':
        before = 'mod child {\nuse super::group::{A, B};\n}\n'
        after = 'mod child {\nuse new::group::B;\nuse new::group::A;\n}\n'
        maps = ['app::group::=new::group::']
    files = {'crates/app/Cargo.toml': '[package]\nname = "app"\n',
             'crates/app/src/lib.rs': HELPER + before}
    if mode == 'nested':
        files.update({'crates/old/Cargo.toml': '[package]\nname = "old"\n',
                      'crates/new/Cargo.toml': '[package]\nname = "new"\n',
                      'crates/old/src/other.rs': HELPER})
    commit_tree(repo, files, 'base')
    changed = {'crates/app/src/lib.rs': HELPER + after}
    if mode == 'nested':
        (repo / 'crates/old/src/other.rs').unlink()
        changed['crates/new/src/other.rs'] = HELPER
    commit_tree(repo, changed, 'imports')
    args = [arg for declaration in maps for arg in ('--rewrite', declaration)]
    code, out = run_checker(repo, *args)
    ok = code == int(mode not in positive)
    if mode not in positive:
        # A mismatching file must not partially bless even its good leaves.
        ok &= field(out, 'path rewrites') == 0
    return ok, f'exit={code} {out.splitlines()[0] if out else "no output"}'


def case_verified_consumers(repo: Path, kind='rerun'):
    package = '[package]\nname = "old"\nversion = "0.1.0"\n'
    old = 'crates/old/src/worker.rs'
    new = 'crates/new/src/worker.rs'
    before = {'Cargo.toml': '[workspace]\nmembers = ["crates/*"]\n',
              'crates/old/Cargo.toml': package, old: HELPER}
    after = {'crates/new/Cargo.toml': package.replace('"old"', '"new"'), new: HELPER}
    if kind == 'rerun':
        before['crates/old/build.rs'] = 'fn main() { println!("cargo:rerun-if-changed=src/worker.rs"); }\n'
        after['crates/old/build.rs'] = before['crates/old/build.rs'].replace('=src/', '=../new/src/')
    else:
        dependency = '[target.\'cfg(windows)\'.dependencies.codec]\nversion = "1"\nfeatures = ["read"]\n'
        before['crates/old/Cargo.toml'] += dependency
        after['crates/new/Cargo.toml'] += dependency
    commit_tree(repo, before, 'base')
    (repo / old).unlink()
    commit_tree(repo, after, 'verified move')
    code, out = run_checker(repo)
    return code == 0, f'exit={code} {out.splitlines()[0] if out else "no output"}'


def review_cases():
    cases = []
    def probe(name, before, after, maps=(), deleted=()):
        def check(repo):
            commit_tree(repo, before, 'base')
            for path in deleted:
                (repo / path).unlink()
            commit_tree(repo, after, 'attempt')
            code, out = run_checker(repo, *[arg for m in maps for arg in ('--rewrite', m)])
            return code == 1, f'exit={code} {out.splitlines()[0] if out else "no output"}'
        cases.append(('review ' + name, check))
    P='crates/app/src/lib.rs'
    M='crates/app/Cargo.toml'
    BASE={'Cargo.toml':'[workspace]\nmembers = ["crates/*"]\n',M:'[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n'}
    def same(name,a,b,maps=('app::group::=app::other::',),path=P,extra=None):
        probe(name,BASE|{path:a}|(extra or {}),{path:b},maps)
    # Whitespace is runtime data in raw strings and significant in Python.
    same('raw_indent','const S: &str = r#"\nhello\n"#;\n','const S: &str = r#"\n  hello\n"#;\n')
    same('string_internal_space','const S: &str = "a b";\n','const S: &str = "ab";\n')
    same('python_indent','def f(flag):\n    if flag:\n        print("yes")\n    print("always")\n','def f(flag):\n    if flag:\n        print("yes")\n        print("always")\n',path='script.py')
    # Context resolves crate-prefixed identifiers as external names, ignoring shadowing.
    common='mod group { pub const VALUE: u32 = 1; }\nmod app { pub mod group { pub const VALUE: u32 = 2; } }\n'
    same('shadow_terminal',common+'pub fn run() -> u32 { crate::group::VALUE }\n',common+'pub fn run() -> u32 { app::group::VALUE }\n',('app::group::=app::group::',))
    common='mod group { pub struct Thing(pub u8); }\nmod app { pub mod group { pub struct Thing(pub u16); } }\n'
    same('shadow_signature',common+'pub fn run(x: crate::group::Thing) -> usize { size_of_val(&x) }\n',common+'pub fn run(x: app::group::Thing) -> usize { size_of_val(&x) }\n',('app::group::=app::group::',))
    common='mod group { pub struct Thing; }\nmod app { pub mod group { pub struct Thing; } }\n'
    same('shadow_impl',common+'impl crate::group::Thing { pub fn run() -> u32 { 1 } }\n',common+'impl app::group::Thing { pub fn run() -> u32 { 1 } }\n',('app::group::=app::group::',))
    common='mod group { pub const VALUE: u32 = 1; }\nmod app { pub mod group { pub const VALUE: u32 = 2; } }\n'
    same('shadow_import',common+'use crate::group::VALUE;\npub fn run() -> u32 { VALUE }\n',common+'use app::group::VALUE;\npub fn run() -> u32 { VALUE }\n',('app::group::=app::group::',))
    same('macro_stringify','pub const S: &str = stringify!(crate::group::Thing);\n','pub const S: &str = stringify!(crate::other::Thing);\n')
    same('macro_recorded_argument','old::emit! { value: 42 }\n','new::emit! { value: 99 }\n',('old::emit!::=new::emit!::',))
    same('macro_import_tree','pub const S: &str = stringify!(use old::group::{A, B};);\n','pub const S: &str = stringify!(use new::group::B; use new::group::A;);\n',('old::group::=new::group::',))
    same('literal_path','pub const S: &str = "crate::group::Thing";\n','pub const S: &str = "crate::other::Thing";\n')
    same('cfg_import','#[cfg(unix)]\nuse old::group::A;\n#[cfg(windows)]\nuse old::group::B;\n','#[cfg(windows)]\nuse new::group::A;\n#[cfg(unix)]\nuse new::group::B;\n',('old::group::=new::group::',))
    # A glob can import newly visible symbols despite preserving the '*' leaf.
    common='mod group { pub const A: u32 = 1; }\nmod other { pub const A: u32 = 1; pub const B: u32 = 2; }\nmod fallback { pub const B: u32 = 3; }\nuse fallback::*;\n'
    same('glob_shadow',common+'mod inner {\nuse crate::group::*;\npub fn run() -> u32 { B }\n}\n',common+'mod inner {\nuse crate::other::*;\npub fn run() -> u32 { B }\n}\n')
    # moved file fixture establishes a donor and a recorded exact source rename.
    OLD='crates/old/src/worker.rs'; NEW='crates/new/src/worker.rs'
    PKG='[package]\nname = "old"\nversion = "0.1.0"\n'
    PAD=HELPER
    MOVEBASE={'Cargo.toml':BASE['Cargo.toml'],'crates/old/Cargo.toml':PKG,OLD:PAD}
    def move(name,extraold=None,extranew=None,oldman='',newman='',maps=(),bodyold='',bodynew=''):
        before=MOVEBASE|{OLD:PAD+bodyold,'crates/old/Cargo.toml':PKG+oldman}|(extraold or {})
        after={NEW:PAD+bodynew,'crates/new/Cargo.toml':PKG.replace('"old"','"new"')+newman}|(extranew or {})
        probe(name,before,after,maps,(OLD,))
    move('build_arbitrary_string',{'crates/old/build.rs':'fn main() {\nlet messages = [\n    "src/worker.rs",\n];\nprintln!("cargo:rustc-env=LABEL={}", messages[0]);\n}\n'}, {'crates/old/build.rs':'fn main() {\nlet messages = [\n    "../new/src/worker.rs",\n];\nprintln!("cargo:rustc-env=LABEL={}", messages[0]);\n}\n'})
    for macro in ('include_str','include_bytes'):
        move(macro+'_different_bytes',{'crates/old/a.txt':'ONE\n','crates/new/a.txt':'TWO\n'},bodyold=f'const A: &str = {macro}!("../a.txt");\n',bodynew=f'const A: &str = {macro}!("../a.txt");\n')
        move(macro+'_wrong_repath',{'crates/old/a.txt':'ONE\n','crates/new/b.txt':'TWO\n'},bodyold=f'const A: &str = {macro}!("../a.txt");\n',bodynew=f'const A: &str = {macro}!("../b.txt");\n')
    LOCK='version = 4\n\n[[package]]\nname = "old"\nversion = "0.1.0"\ndependencies = ["codec 1.0.0"]\n\n[[package]]\nname = "codec"\nversion = "1.0.0"\nsource = "registry+https://example.com"\n'
    WIRING='[dependencies]\ncodec = "1"\n'
    for kind in ('version','source','new-version','feature','target','dependency'):
        oldman=WIRING; newman=WIRING
        newlock=LOCK+'\n[[package]]\nname = "new"\nversion = "0.1.0"\ndependencies = ["codec 1.0.0"]\n'
        if kind=='version': newlock=newlock.replace('version = "1.0.0"','version = "2.0.0"')
        if kind=='source': newlock=newlock.replace('https://example.com','https://evil.com')
        if kind=='new-version': newlock=newlock.replace('dependencies = ["codec 1.0.0"]\n', 'dependencies = ["codec 2.0.0"]\n',1)
        if kind=='feature': newman='[dependencies]\ncodec = { version = "1", features = ["evil"] }\n'
        if kind=='target':
            oldman="[target.'cfg(unix)'.dependencies]\ncodec = \"1\"\n"; newman=oldman.replace('unix','windows')
        if kind=='dependency': newman+='evil = "9"\n'
        move('metadata_'+kind,{'Cargo.lock':LOCK},{'Cargo.lock':newlock},oldman,newman)
    common='mod group { pub const A: u32 = 1; }\nmod other { pub const A: u32 = 1; pub const B: u32 = 2; }\nmod fallback { pub const B: u32 = 3; }\nuse fallback::*;\n'
    same('glob_shadow_valid',common+'pub fn run() -> u32 {\nuse crate::group::*;\nB\n}\n',common+'pub fn run() -> u32 {\nuse crate::other::*;\nB\n}\n')
    same('macro_import_tree_adjacent','pub const S: &str = stringify!(use old::group::{A, B};);\n','pub const S: &str = stringify!(use new::group::B;use new::group::A;);\n',('old::group::=new::group::',))
    move('include_bytes_different_bytes_valid',{'crates/old/a.txt':'ONE\n','crates/new/a.txt':'TWO\n'},bodyold='const A: &[u8] = include_bytes!("../a.txt");\n',bodynew='const A: &[u8] = include_bytes!("../a.txt");\n')
    move('metadata_target_dotted_escape',oldman="[target.'cfg(windows)'.dependencies]\ncodec = { version = \"1\", features = [\"evil\"] }\n",newman='[dependencies.codec]\nversion = "1"\nfeatures = ["evil"]\n')
    same('cfg_parent_move','#[cfg(unix)]\nmod first {\nuse old::group::A;\n}\n#[cfg(windows)]\nmod second {\nuse old::group::B;\n}\n','#[cfg(windows)]\nmod first {\nuse new::group::A;\n}\n#[cfg(unix)]\nmod second {\nuse new::group::B;\n}\n',('old::group::=new::group::',))
    common='mod group;\nmod app { pub mod other { pub const VALUE: u32 = 2; } }\n'
    probe('shadow_actual_rename',BASE|{P:common+'pub fn run() -> u32 { crate::group::VALUE }\n','crates/app/src/group.rs':'pub const VALUE: u32 = 1;\n'}, {P:common.replace('mod group;', 'mod other;')+'pub fn run() -> u32 { app::other::VALUE }\n','crates/app/src/other.rs':'pub const VALUE: u32 = 1;\n'},('app::group::=app::other::',),('crates/app/src/group.rs',))
    same('shadow_self', 'mod group { pub const VALUE: u32 = 1; }\nmod app { pub mod group { pub const VALUE: u32 = 2; } }\npub fn run() -> u32 { self::group::VALUE }\n','mod group { pub const VALUE: u32 = 1; }\nmod app { pub mod group { pub const VALUE: u32 = 2; } }\npub fn run() -> u32 { app::group::VALUE }\n',('app::group::=app::group::',))
    common='mod group { pub const VALUE: u32 = 1; }\nmod child {\nmod app { pub mod group { pub const VALUE: u32 = 2; } }\n'
    same('shadow_super',common+'pub fn run() -> u32 { super::group::VALUE }\n}\n',common+'pub fn run() -> u32 { app::group::VALUE }\n}\n',('app::group::=app::group::',))
    move('metadata_feature_line',oldman='[features]\nproof = []\nperf = ["proof"]\n',newman='[features]\nproof = []\nperf = []\n')
    for macro in ('format', 'concat', 'arbitrary'):
        same('macro_' + macro,
             f'const S: &str = {macro}!(crate::group::Thing);\n',
             f'const S: &str = {macro}!(crate::other::Thing);\n')
    same('macro_multiline_import',
         'stringify! {\nuse old::group::{A, B};\n}\n',
         'stringify! {\nuse new::group::B;\nuse new::group::A;\n}\n',
         ('old::group::=new::group::',))
    same('cfg_item_swap',
         '#[cfg(unix)]\nfn first() {}\n#[cfg(windows)]\nfn second() {}\n',
         '#[cfg(windows)]\nfn first() {}\n#[cfg(unix)]\nfn second() {}\n')
    for path in ('data.yaml', 'Makefile', 'unknown.data'):
        same('indent_' + path, 'root:\n  child: value\n', 'root:\nchild: value\n', path=path)
    for name, call in (
            ('build_label', 'native_source_identity::emit_source_identity(&root, &[], "src/worker.rs");'),
            ('build_arg', 'command.arg("src/worker.rs");')):
        move(name, {'crates/old/build.rs': 'fn main() { ' + call + ' }\n'},
             {'crates/old/build.rs': 'fn main() { ' + call.replace('src/worker.rs', '../new/src/worker.rs') + ' }\n'})
    same('macro_reordered_lines',
         'stringify! {\nuse old::group::A;\nuse old::group::B;\n}\n',
         'stringify! {\nuse old::group::B;\nuse old::group::A;\n}\n')
    same('cfg_item_drop', '#[cfg(unix)]\nfn first() {}\n', 'fn first() {}\n')
    same('cfg_inner_swap',
         'mod first {\n#![cfg(unix)]\nuse old::group::A;\n}\nmod second {\n#![cfg(windows)]\nuse old::group::B;\n}\n',
         'mod first {\n#![cfg(windows)]\nuse new::group::A;\n}\nmod second {\n#![cfg(unix)]\nuse new::group::B;\n}\n',
         ('old::group::=new::group::',))
    return cases


CASES = review_cases() + [
    ('verified rerun source consumer', case_verified_consumers),
    ('verified dotted target dependency', lambda repo: case_verified_consumers(repo, 'target')),
] + [
    (f'import identity {mode}', lambda repo, mode=mode: case_import_identity(repo, mode))
    for mode in ('split', 'merge', 'reorder', 'nested', 'attributes', 'mixed', 'inline', 'raw-absolute',
                 'drop', 'add', 'alias', 'visibility', 'glob', 'cfg-between-leaves',
                 'wrong-symbol', 'duplicate', 'duplicate-visibility', 'attribute-change', 'mixed-body', 'malformed-body', 'scope')
] + [
    (f'context {kind}, {"negative" if bad else "positive"}',
     lambda repo, kind=kind, bad=bad: case_context_move(repo, kind, bad))
    for kind in ('path', 'terminal', 'impl', 'inline', 'glob-inline', 'macro', 'macro-local', 'macro-call', 'macro-rooted', 'macro-chain', 'hygiene',
                 'include-str', 'include-bytes', 'documented-include', 'literal', 'continued-string', 'opaque', 'build', 'build-raw')
    for bad in (False, True)
] + [
    (f'move metadata {kind}, {"negative" if bad else "positive"}',
     lambda repo, kind=kind, bad=bad: case_move_metadata(repo, kind, bad))
    for kind in ('target', 'target-from-normal', 'feature', 'dependency', 'lock', 'lock-duplicate', 'forward') for bad in (False, True)
] + [
    (f"bin source {mode}, {'existing' if existing else 'new'} crate",
     lambda repo, mode=mode, existing=existing: case_bin_source_transfer(repo, mode, existing))
    for existing in (False, True)
    for mode in ("different", "identical", "rename", "rename-rewrite", "missing", "line-endings", "empty")
] + [
    ("bin transfer with reformatted table", case_bin_transfer),
    ("bin transfer with default path", lambda repo: case_bin_transfer(repo, "default")),
    ("bin changed path is residue", lambda repo: case_bin_transfer(repo, "path")),
    ("bin added key is residue", lambda repo: case_bin_transfer(repo, "key")),
    ("bin changed features is residue", lambda repo: case_bin_transfer(repo, "features")),
    ("bin changed name is residue", lambda repo: case_bin_transfer(repo, "name")),
    ("bin without removal is residue", lambda repo: case_bin_transfer(repo, "retained")),
    ("bin dropping old key is residue", lambda repo: case_bin_transfer(repo, "old-key")),
    ("multiple bin tables transfer independently", lambda repo: case_bin_transfer(repo, "multiple")),
    ("bin removal cannot authorize two additions", lambda repo: case_bin_transfer(repo, "duplicate")),
    ("external dependency multiline table", case_external_dependency),
    ("external dependency inline reformatted", lambda repo: case_external_dependency(repo, "inline")),
    ("external dependency from workspace root", lambda repo: case_external_dependency(repo, "workspace-root")),
    ("external dependency table from inline precedent", lambda repo: case_external_dependency(repo, "inline-precedent")),
    ("external dev dependency table", lambda repo: case_external_dependency(repo, table="dev-dependencies")),
    ("external build dependency table", lambda repo: case_external_dependency(repo, table="build-dependencies")),
    ("external dependency changed version is residue", lambda repo: case_external_dependency(repo, "version")),
    ("external dependency changed features is residue", lambda repo: case_external_dependency(repo, "features")),
    ("external dependency changed name is residue", lambda repo: case_external_dependency(repo, "name")),
    ("external dependency nonmember precedent is residue", lambda repo: case_external_dependency(repo, "nonmember")),
    ("new crate skeletons", case_crate_skeleton),
    ("new lib.rs with function is residue", lambda repo: case_crate_skeleton(repo, True)),
    ("new manifest with bounded dependencies", case_new_manifest),
    ("new manifest build script is residue", lambda repo: case_new_manifest(repo, "build")),
    ("new manifest default features are residue", lambda repo: case_new_manifest(repo, "default")),
    ("new manifest unproven external dependency is residue", lambda repo: case_new_manifest(repo, "external")),
    ("new manifest dependency feature selection is residue", lambda repo: case_new_manifest(repo, "dep-features")),
    ("new manifest unsupported table is residue", lambda repo: case_new_manifest(repo, "table")),
    ("manifest wiring additions", case_manifest),
    ("manifest wiring removals", lambda repo: case_manifest(repo, "remove")),
    ("remove feature-selected dependency entirely", lambda repo: case_dependency_selector_removal(repo, gone=True)),
    ("drop features from surviving dependency is residue", case_dependency_selector_removal),
    ("drop default-features from surviving dependency is residue", lambda repo: case_dependency_selector_removal(repo, "default-features")),
    ("drop optional from surviving dependency is residue", lambda repo: case_dependency_selector_removal(repo, "optional")),
    ("dependency survives in another table", lambda repo: case_dependency_selector_removal(repo, moved_table=True)),
    ("existing manifest inherits workspace lints", case_manifest_carveout),
    ("existing lint table with second key is residue", lambda repo: case_manifest_carveout(repo, near_miss=True)),
    ("new manifest inherits workspace lints", lambda repo: case_manifest_carveout(repo, new=True)),
    ("new lint table with second key is residue", lambda repo: case_manifest_carveout(repo, new=True, near_miss=True)),
    ("new crate defines empty testkit feature", lambda repo: case_manifest_carveout(repo, "empty-feature", new=True)),
    ("existing crate empty feature is residue", lambda repo: case_manifest_carveout(repo, "empty-feature")),
    ("added dev-dependency selects testkit", lambda repo: case_manifest_carveout(repo, "dev-features")),
    ("new manifest dev-dependency selects testkit", lambda repo: case_manifest_carveout(repo, "dev-features", new=True)),
    ("normal dependency selecting testkit is residue", lambda repo: case_manifest_carveout(repo, "dev-features", near_miss=True)),
    ("build dependency selecting testkit is residue", lambda repo: case_manifest_carveout(repo, "build-features")),
    ("default feature forwarding is residue", lambda repo: case_manifest(repo, "default")),
    ("path dependency features are residue", lambda repo: case_manifest(repo, "features")),
    ("path dependency default-features is residue", lambda repo: case_manifest(repo, "default-features")),
    ("path dependency optional is residue", lambda repo: case_manifest(repo, "optional")),
    ("workspace member names an existing manifest", case_members),
    ("workspace member removal removes directory", lambda repo: case_members(repo, "remove")),
    ("workspace member removal retaining crate is residue", lambda repo: case_members(repo, "remove-kept")),
    ("workspace member removal retaining files is residue", lambda repo: case_members(repo, "remove-leftover")),
    ("workspace member without manifest is residue", lambda repo: case_members(repo, "missing")),
    ("manifest version bump is residue", lambda repo: case_manifest(repo, "version")),
    ("local feature is residue", lambda repo: case_manifest(repo, "local-feature")),
    ("external path dependency is residue", lambda repo: case_manifest(repo, "external-path")),
    ("explicit path rewrites", case_path_rewrite),
    ("rewrite indentation change", lambda repo: case_path_rewrite(repo, "spacing")),
    ("rewrite string losing a space is residue", lambda repo: case_path_rewrite(repo, "string-space")),
    ("rewrite cannot consume an already moved line", lambda repo: case_path_rewrite(repo, "moved-collision")),
    ("rewritten argument change is residue", lambda repo: case_path_rewrite(repo, "argument")),
    ("no implicit rewrites", lambda repo: case_path_rewrite(repo, "implicit")),
    ("rewrites cannot pair separate files", lambda repo: case_path_rewrite(repo, "cross-file")),
    ("rewrites respect path boundaries", lambda repo: case_path_rewrite(repo, "boundary")),
    ("git-detected rename with rewrite", case_renamed_rewrite),
    ("git-detected rename with argument edit is residue", lambda repo: case_renamed_rewrite(repo, True)),
    ("delete/add rewrite gives separate-commits hint", case_deleted_added_rewrite),
    ("delete/add rewrite plus argument change stays residue", lambda repo: case_deleted_added_rewrite(repo, True)),
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
