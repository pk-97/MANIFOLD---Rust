#!/usr/bin/env python3
"""Self-test for move_identity_check.py — the pure-move gate.

Each case builds a throwaway git repo, commits a "before" tree, commits an
"after" tree that relocates code, and asserts the checker's verdict (exit 0 =
PURE MOVE PROVEN, exit 1 = residue). Run: `python3 scripts/test_move_identity_check.py`.

Cases 1-5 lock in the classes the P-P landing established (plain move, smuggled
edit caught, visibility widening, comment lines, mod/use wiring). Case 6 is the
P-F2a addition (D-15): a bare inherent-impl wrapper is ALLOW-class wiring, but a
body edit hiding inside that wrapper is still caught.
"""
import os
import subprocess
import sys
import tempfile

CHECKER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "move_identity_check.py")


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)


def write_tree(d, tree):
    for name, content in tree.items():
        with open(os.path.join(d, name), "w") as f:
            f.write(content)


def run_case(name, before, after, expect_pure):
    with tempfile.TemporaryDirectory() as d:
        git(d, "init", "-q")
        git(d, "config", "user.email", "t@example.com")
        git(d, "config", "user.name", "Test")
        write_tree(d, before)
        git(d, "add", "-A")
        git(d, "commit", "-qm", "before")
        for f in before:
            if f not in after:
                os.remove(os.path.join(d, f))
        write_tree(d, after)
        git(d, "add", "-A")
        git(d, "commit", "-qm", "after")
        r = subprocess.run(
            [sys.executable, CHECKER, "HEAD"], cwd=d, capture_output=True, text=True
        )
        is_pure = r.returncode == 0
        ok = is_pure == expect_pure
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: exit={r.returncode} "
              f"(expected {'PROVEN' if expect_pure else 'residue'})")
        if not ok:
            print("--- checker output ---")
            print(r.stdout)
            print(r.stderr)
        return ok


FN_A = (
    "    fn a(&self) -> u32 {\n"
    "        let x = 1;\n"
    "        let y = 2;\n"
    "        x + y\n"
    "    }\n"
)


def fn_b(q_val=20):
    return (
        "    fn b(&self) -> u32 {\n"
        "        let p = 10;\n"
        f"        let q = {q_val};\n"
        "        p + q\n"
        "    }\n"
    )


def main():
    ok = True

    # 1. Pure whole-fn move from one module to another.
    ok &= run_case(
        "pure move",
        {"lib.rs": "mod a;\nmod b;\n",
         "a.rs": FN_A,
         "b.rs": fn_b()},
        {"lib.rs": "mod a;\nmod b;\n",
         "a.rs": "",
         "b.rs": FN_A + "\n" + fn_b()},
        expect_pure=True,
    )

    # 2. Smuggled edit inside the moved block (q: 20 -> 30) is caught.
    ok &= run_case(
        "smuggled edit caught",
        {"a.rs": FN_A + "\n" + fn_b(20)},
        {"a.rs": FN_A, "b.rs": fn_b(30)},
        expect_pure=False,
    )

    # 3. Visibility widening on the moved fn (fn -> pub(crate) fn).
    ok &= run_case(
        "visibility widening",
        {"a.rs": FN_A + "\n" + fn_b()},
        {"a.rs": FN_A, "b.rs": "    pub(crate) " + fn_b().lstrip()},
        expect_pure=True,
    )

    # 4. A comment line added alongside the move is counted, not fatal.
    ok &= run_case(
        "comment line non-fatal",
        {"a.rs": FN_A + "\n" + fn_b()},
        {"a.rs": FN_A, "b.rs": "    // relocated helper\n" + fn_b()},
        expect_pure=True,
    )

    # 5. mod / use wiring added alongside the move.
    ok &= run_case(
        "mod/use wiring",
        {"lib.rs": "fn root() {}\n", "a.rs": fn_b()},
        {"lib.rs": "mod b;\nuse crate::b::*;\nfn root() {}\n", "a.rs": "", "b.rs": fn_b()},
        expect_pure=True,
    )

    # 6a. Bare inherent-impl wrapper move (D-15): methods relocate into a fresh
    #     `impl Foo {` in a submodule — PROVEN.
    before_impl = {
        "mod.rs": "struct Foo;\nimpl Foo {\n" + FN_A + "\n" + fn_b() + "}\n",
    }
    after_impl = {
        "mod.rs": "struct Foo;\n\nmod overlay;\n\nimpl Foo {\n" + FN_A + "}\n",
        "overlay.rs": "use super::*;\n\nimpl Foo {\n" + fn_b() + "}\n",
    }
    ok &= run_case("impl-wrapper move", before_impl, after_impl, expect_pure=True)

    # 6b. A body edit hiding inside that same wrapper is still caught.
    after_impl_edit = {
        "mod.rs": "struct Foo;\n\nmod overlay;\n\nimpl Foo {\n" + FN_A + "}\n",
        "overlay.rs": "use super::*;\n\nimpl Foo {\n" + fn_b(30) + "}\n",
    }
    ok &= run_case("impl-wrapper body edit caught", before_impl, after_impl_edit, expect_pure=False)

    print("\n" + ("ALL PASS" if ok else "FAILURES ABOVE"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
