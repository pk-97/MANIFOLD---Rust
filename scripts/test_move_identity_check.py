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
  8. D-11 preamble move   → exit 0, residue 0, scaffold>0 (byte-exact 2-line
                                                           preamble recomputed atop
                                                           a moved dispatch_<d> fn)
  9. D-11 deviated        → exit 1, residue > 0          (one token off the
     preamble                                            byte-exact form — any
                                                           deviation = residue)
 10. impl-wrapper move    → exit 0, residue 0            (D-15: a bare inherent-impl
                                                           wrapper relocated into a
                                                           submodule is ALLOW wiring)
 11. impl-wrapper body    → exit 1, residue > 0          (D-15: a body edit hiding
     edit                                                 inside the moved wrapper)

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


CASES = [
    ("pure move -> exit 0", case_pure_move),
    ("smuggled edit -> exit 1", case_smuggled),
    ("dispatch-split scaffold -> exit 0", case_dispatch_split),
    ("dropped arm -> exit 1", case_dropped_arm),
    ("scaffold over cap -> exit 1", case_over_cap),
    ("multi-line use move -> exit 0 [D-18]", case_multiline_use_move),
    ("smuggled use-block -> exit 1 [D-18]", case_smuggled_use_block),
    ("D-11 preamble move -> exit 0, scaffold [PROVEN]", case_preamble_scaffold),
    ("D-11 deviated preamble -> exit 1 [CAUGHT]", case_preamble_deviated),
    ("impl-wrapper move -> exit 0 [D-15]", case_impl_wrapper_move),
    ("impl-wrapper body edit -> exit 1 [D-15]", case_impl_wrapper_body_edit),
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
