#!/usr/bin/env python3
"""Batch-landing self-tests on throwaway local repos with a stub gate."""
import contextlib
import io
import os
import subprocess
import tempfile
import unittest

import land_wave

GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t"]


def git(*a, cwd=None):
    return subprocess.run(GIT + list(a), cwd=cwd, text=True, capture_output=True, check=True).stdout.strip()


class BatchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = self.tmp.name
        self.origin, self.work = f"{t}/origin.git", f"{t}/work"
        git("init", "-q", "--bare", "-b", "main", self.origin)
        git("clone", "-q", self.origin, self.work)
        self.write("base.txt", "base")
        git("add", "base.txt", cwd=self.work)
        git("commit", "-q", "-m", "base", cwd=self.work)
        git("push", "-q", "origin", "HEAD:main", cwd=self.work)
        git("fetch", "-q", "origin", cwd=self.work)
        self.gate = f"{t}/gate.sh"
        with open(self.gate, "w") as f:
            f.write("#!/bin/sh\n[ -f bad.txt ] && { echo 'bad.txt present'; exit 1; }\nexit 0\n")
        os.chmod(self.gate, 0o755)
        self.old = os.getcwd()
        os.chdir(self.work)

    def tearDown(self):
        os.chdir(self.old)
        self.tmp.cleanup()

    def write(self, name, text):
        with open(f"{self.work}/{name}", "w") as f:
            f.write(text)

    def branch(self, name, fname, text="x"):
        git("checkout", "-q", "-b", name, "origin/main", cwd=self.work)
        self.write(fname, text)
        git("add", fname, cwd=self.work)
        git("commit", "-q", "-m", name, cwd=self.work)
        return git("rev-parse", "HEAD", cwd=self.work)

    def land(self, specs):
        with contextlib.redirect_stdout(io.StringIO()):
            return land_wave.land_batch(specs, [self.gate], "Batch")

    def test_order_and_message_listing(self):
        ta = self.branch("a", "a.txt")
        tb = self.branch("b", "b.txt")
        sha, inc, dropped = self.land([("a", None), ("b", None)])
        self.assertEqual(inc, [("a", ta), ("b", tb)])
        self.assertEqual(dropped, [])
        self.assertEqual(git("rev-parse", "origin/main", cwd=self.work), sha)
        msg = git("log", "-1", "--format=%B", sha, cwd=self.work)
        self.assertLess(msg.index(f"a {ta}"), msg.index(f"b {tb}"))
        self.assertEqual(len(git("rev-list", "--parents", "-1", sha, cwd=self.work).split()), 3)
        for f in ("a.txt", "b.txt"):
            git("cat-file", "-e", f"{sha}:{f}", cwd=self.work)

    def test_pinned_tip(self):
        t1 = self.branch("a", "a.txt")
        self.write("later.txt", "later")
        git("add", "later.txt", cwd=self.work)
        git("commit", "-q", "-m", "later", cwd=self.work)
        sha, inc, _ = self.land([("a", t1)])
        self.assertEqual(inc, [("a", t1)])
        self.assertNotEqual(subprocess.run(
            GIT + ["cat-file", "-e", f"{sha}:later.txt"], cwd=self.work).returncode, 0)

    def test_drop_on_red(self):
        self.branch("a", "a.txt")
        tb = self.branch("b", "bad.txt")
        self.branch("c", "c.txt")
        sha, inc, dropped = self.land([("a", None), ("b", None), ("c", None)])
        self.assertEqual([n for n, _ in inc], ["a", "c"])
        self.assertEqual(dropped[0][0], "b")
        self.assertEqual(dropped[0][1], tb)
        self.assertIn("bad.txt present", dropped[0][2])
        msg = git("log", "-1", "--format=%B", sha, cwd=self.work)
        self.assertIn("Dropped:", msg)
        self.assertNotEqual(subprocess.run(
            GIT + ["cat-file", "-e", f"{sha}:bad.txt"], cwd=self.work).returncode, 0)

    def test_conflict_dropped(self):
        self.branch("a", "same.txt", "one")
        self.branch("b", "same.txt", "two")
        _, inc, dropped = self.land([("a", None), ("b", None)])
        self.assertEqual([n for n, _ in inc], ["a"])
        self.assertEqual(dropped[0][0], "b")

    def test_unfixable_red_lands_nothing(self):
        self.branch("a", "bad.txt")
        self.branch("b", "bad2.txt")
        git("checkout", "-q", "b", cwd=self.work)
        git("mv", "bad2.txt", "bad.txt", cwd=self.work)
        git("commit", "-q", "-m", "rename", cwd=self.work)
        before = git("rev-parse", "origin/main", cwd=self.work)
        with self.assertRaises(SystemExit):
            self.land([("a", None), ("b", None)])
        self.assertEqual(git("ls-remote", self.origin, "main").split()[0], before)


if __name__ == "__main__":
    unittest.main()
