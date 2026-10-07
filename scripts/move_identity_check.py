#!/usr/bin/env python3
"""Move-identity verifier — the pure-move gate for the god-file decomposition waves.

A "pure move" commit relocates code without changing it. This script proves it
mechanically: it runs `git diff --color-moved` with pinned colors and counts
every added/removed line that git did NOT classify as moved. For a pure-move
commit the non-moved residue must be ZERO after the allowlist (module wiring:
`mod`/`use`/`pub use` lines, blank lines, module doc comments, diff headers,
and test-mod headers — `#[cfg(test)]` + `mod <name> {`/`}` — when tests are
distributed across the new submodules (D7a, 1-old→N-new) or one inline
`#[cfg(...)] mod X { … }` is converted to a `mod X;` declaration + sibling file
(W3-D2, 1-to-1; the `#[path = "…"]` tests-out form included)).
Crate moves add three separately counted classes: newly added
`crates/<name>/Cargo.toml` and wiring-only `src/lib.rs`/`src/main.rs` skeletons;
workspace members, local path dependencies and feature-forwarding manifest
entries; and same-file removed/added pairs matching explicit --rewrite maps.
Rewrite counts are changed lines (two per exact pair, or a complete import
group). Each file pair's use-leaf multisets must match after rewriting; scope,
visibility, attributes, aliases and glob/named identities are preserved.
Declared paths resolve in each
revision's crate and file/inline-module context, including terminal symbols,
macro arguments and $crate. Exported macro maps use OLD::macro!::=NEW::macro!::.
Strings/comments are opaque. Includes and build-source rows follow exact git
rename pairs; includes additionally require asset byte identity. New-package
lock entries and target dependencies/features require source-crate precedents.
Unmatched contextual paths stay residue even when shaped like legacy wiring.
New-manifest external dependencies equal to parsed old-workspace values and
transferred bin tables count as crate skeletons; transfers in existing manifests
count as manifest wiring. Bins preserve name, crate-relative path (defaulting to
src/bin/<name>.rs), and required-features, with no other table keys allowed.
Their sources must be a git rename pair or byte-identical across revisions.
Exit code 0 = pure move proven; 1 = residue found (printed); 2 = usage.

Why not `cargo public-api`: not installed, requires a lib target (manifold-app
is bin-only — most of Wave 1 is invisible to it), and moving a pub item across
modules legitimately changes its path. This script sees every line instead.

Usage:
  scripts/move_identity_check.py <commit>            # one commit vs its parent
  scripts/move_identity_check.py <base>..<head>      # a range
  scripts/move_identity_check.py --cached            # staged changes
  scripts/move_identity_check.py <ref> --show-all    # print all residue lines
  scripts/move_identity_check.py <ref> --rewrite 'crate::node_graph::=manifold_node_engine::'

The moved-line detection uses `--color-moved=plain --color-moved-ws=no` and
pins the four diff colors so parsing never depends on user git config.
Rust indentation is handled by contextual keys; literal bytes, opaque macro
payloads and non-Rust indentation cannot use whitespace-normalized waivers.
Plain mode marks individual matching added/removed lines; it has no three-line
minimum. Unmatched lines remain subject to the bounded classes below and review.
"""

import re
import subprocess
import sys
import argparse
import fnmatch
import json
import posixpath
import tomllib
from collections import Counter

# Pinned colors: 35=magenta (old moved), 36=cyan (new moved). git may emit
# them with attributes (e.g. \x1b[1;36m), so match the code anywhere in the
# leading escape sequence of the line.
MOVED_RE = re.compile(r"^\x1b\[(?:[0-9;]*;)?3[56](?:;[0-9;]*)?m")
ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Module-wiring lines a pure move is allowed to add/remove.
ALLOW = re.compile(
    r"^[+-]\s*("
    r"$"                                        # blank
    r"|(pub(\((crate|super)\))?\s+)?mod\s+\w+;" # mod / pub mod decl
    r"|(pub(\((crate|super)\))?\s+)?use\s"      # use / pub use
    r"|#\[path\s*=\s*\".*\"\]"                  # #[path] attr on a mod decl
    r"|//!"                                     # module-level doc comment
    r"|impl\s+\w+\s*\{\s*$"                     # bare inherent-impl wrapper
    r")"
)
# The inherent-impl wrapper case (P-F2a, god-file wave): relocating methods into
# a submodule needs a fresh `impl UIRoot {` line the source never removed — pure
# structural scaffolding, behavior-neutral, exactly like the `mod`/`use` wiring
# above. Kept deliberately TIGHT: `impl <Name> {` on its own line only. A trait
# impl (`impl X for Y {`) has ` for ` after the name and does NOT match — it
# stays residue by design (a moved trait impl is rare enough to justify in
# review). A same-line body (`impl Foo { fn ... }`) fails the `\s*$` and stays
# residue too, so a smuggled edit can't hide behind the wrapper. The matching
# bare `}` is already picked up as a move by plain-mode (methods always carry
# removed `}` lines for it to pair against).
HEADER = re.compile(r"^(\+\+\+|---)\s")

# Comment-only lines: behavior-neutral in Rust (nextest does not run doctests
# in this repo's gate). Counted and reported, never fatal.
COMMENT = re.compile(r"^[+-]\s*(//|///|//!)")

# Visibility qualifiers a move may add/remove on an otherwise-identical line —
# widening is required wiring when a private item crosses a module wall, and
# cannot change runtime behavior (it only widens who may call).
VIS = re.compile(r"^(pub(\((crate|super)\))?\s+)")

# Multi-line `use { ... }` brace-list continuation (D-18): the single-line
# ALLOW regex above only matches lines that themselves start with `use`, so
# the inner list lines and the closing `};` of a multi-line import block were
# surfacing as residue even though they are pure wiring. Fixed with STATEFUL
# per-sign block tracking in `classify()`: a +/- line matching USE_OPEN below
# (a `use` line ending in an unclosed `{`) opens a block for that sign; while
# open, a same-sign line is allowed ONLY if it matches USE_ITEM — one or more
# `ident`/`a::b::c` path items, comma-separated, optional `as alias`, optional
# trailing comma, optional closing `};` (which closes the block). Anything
# else inside an open block is NOT import-shaped and falls through to residue
# — this is the smuggle-proofing: a real code statement hidden between the
# `use x::{` opener and the `};` closer must still be caught. Per-sign state
# resets on every non-content diff line (hunk/file header, context line) so a
# stray unrelated line elsewhere in the diff can never inherit an open block —
# UNLESS that context line is itself a `use ... {` opener (D-20 i, see below).
#
# Context-opened blocks (D-20 i): the above only ARMS on a SIGNED opening
# line. An inner-line-only edit under an otherwise-UNCHANGED `use ... {` —
# e.g. one name swapped in a pre-existing multi-line import, opener and
# closer both untouched — never emits the opener as a +/- line, so per-sign
# tracking never arms and the inner +/- lines fell to residue. Fixed with a
# second, SHARED `context_block` flag in `classify()`: a context (unchanged)
# line matching USE_OPEN arms it; while armed it governs BOTH +/- inner
# lines (the opener applies to old and new file alike); a context line
# matching the closer shape disarms it. USE_ITEM's shape check is untouched
# and applies identically to context-armed and signed-armed blocks, so the
# smuggle-proofing (a non-import statement inside an open block is still
# residue) holds for both.
USE_OPEN = re.compile(r"^(pub(\((crate|super)\))?\s+)?use\s.*\{\s*$")
USE_CLOSE = re.compile(r"^\}\s*;?$")
_IDENT = r"[A-Za-z_]\w*"
_PATH_ITEM = rf"{_IDENT}(?:::{_IDENT})*(?:\s+as\s+{_IDENT})?"
USE_ITEM = re.compile(
    rf"^(?:{_PATH_ITEM}(?:\s*,\s*{_PATH_ITEM})*,?\s*(?:\}}\s*;?)?|\}}\s*;?)$"
)

# Test-mod wiring class (D7a, Wave 2 P2-G): distributing one flat
# `#[cfg(test)] mod tests { ... }` into ~7 per-module test mods multiplies the
# header lines — the cfg-test attribute, the `mod <name> {` opener, and the
# closing `}`. git's `--color-moved` USUALLY pairs each repeated header against
# the single removed original, but that is threshold-fragile: when the new mod
# is renamed or its cfg gate differs from the original (e.g. a feature-gated
# `#[cfg(all(test, feature = "…"))]`), the added header line has no identical
# removed counterpart and falls to residue even though it is pure wiring
# (verified: a renamed/feature-gated distribution surfaces exactly the cfg-attr
# and `mod {` opener lines as residue). This class ALLOWS them, SMUGGLE-PROOF:
# the cfg attr is wiring only when the IMMEDIATELY-following same-sign line is a
# `mod <name> {` opener (the attribute must be attached to a mod), and a bare
# `}` is wiring only while a counted test-mod brace is open — any other line
# under the class (a smuggled statement in a test-mod header) falls straight
# through to residue. State + matching live in classify() (per-sign, reset at
# every hunk/file/context boundary like the use-block trackers). The `}` depth
# advance is deliberately confined to NON-moved lines: the distributed test
# BODIES are git-detected moves and never reach it, so their internal braces
# can't desync the counter.
CFG_TEST_ATTR = re.compile(
    r'^#\[cfg\((?:test|all\(\s*test\s*,\s*feature\s*=\s*"[^"]*"\s*\))\)\]$'
)
MOD_OPEN = re.compile(r"^(?:pub(?:\((?:crate|super)\))?\s+)?mod\s+\w+\s*\{$")
BARE_CLOSE = re.compile(r"^\}$")

# Dispatch-split scaffold (UI_FUNNEL_DECOMPOSITION P-B): the structural glue a
# `dispatch_inspector` match-split adds that git cannot classify as a move
# because it is genuinely new text, not relocated — a sub-dispatcher signature,
# its `match action {`, the `unhandled` sentinel, closing braces, and the
# ordered first-non-unhandled CHAIN ROUTER lines (one delegation call + one
# fall-through guard per domain module — bounded by module count, ~2 lines
# each). Counted SEPARATELY and capped (SCAFFOLD_CAP): bulk semantics must
# never hide here, so a commit exceeding the cap FAILS. Deliberately NARROW —
# no delegation-arm or `PanelAction::` pattern of any kind, because a
# hand-written variant→module arm is a routing decision (its correctness is
# proven by variant-census equality, NOT waived here). The chain-router lines
# are per-DOMAIN (~7 total), not per-variant, so they stay well under the cap
# and cannot smuggle a misroute. See docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md
# D6 / INV-G1.
SCAFFOLD_CAP = 25
SCAFFOLD = re.compile(
    r"^[+-]\s*(?:"
    r"(?:pub(?:\((?:crate|super)\))?\s+)?fn dispatch_\w+\(.*"  # (1) sub-dispatcher signature (single line)
    r"|match action \{"                                        # (2) the per-domain match head
    r"|_ => DispatchResult::unhandled\(\),?"                   # (3) the fall-through sentinel
    r"|DispatchResult::unhandled\(\)"                          # (3b) bare tail expr: router
                                                                #      fully collapsed (S6b)
    r"|let \w+ = [\w:]+::dispatch_\w+\(action, ctx\);"         # (5) chain-router delegation call
    r"|if !(\w+)\.unhandled \{ return \1; \}"                  # (6) chain-router fall-through guard
    r"|\}\)?,?;?"                                              # (4) a bare closing brace
    r")\s*$"
)

# D-11 preamble (UI_FUNNEL_DECOMPOSITION P-B, params/modulation/mapping domains): the
# ONLY sanctioned preamble is byte-exact. When a domain is split into its own
# `dispatch_<d>` fn, that fn can't inherit the outer fn's locals and must recompute
# these two lines at its top; later slices delete the now-dead original from
# inspector.rs. So these lines legitimately appear as `+` (new fn) and eventually `-`
# (dead original) with zero behavior change — counted as SCAFFOLD (same SCAFFOLD_CAP),
# never residue. Matched as EXACT-STRING literals, whitespace-normalized (leading
# +/- marker stripped, internal whitespace collapsed to single spaces), deliberately
# NARROW — NOT a general `let ... = super::...` shape. Any deviation (different arg,
# renamed local, reordered call) is NOT in this set and falls straight through to
# residue: "any deviation from the byte-exact form = residue = investigate, never
# adapt" (D-11). Encodes D-11's text as the truth, not whatever inspector.rs happens
# to contain today.
#
# Drifted removal-side SEQUENCE (D-21, replacing D-20 iii's per-line entries):
# inspector.rs's ACTUAL in-source preamble (still in `dispatch_inspector`,
# verified against the source at the time of the D-20 iii fix) drifted from
# the canonical form above — an explicit `&*ctx.active_layer` reborrow on the
# call's last arg, an explicit `&Option<LayerId>` type annotation on the
# second `let`, and the call formatted across multiple lines rather than one.
#
# D-20 iii originally registered each drifted source line as an independent
# PREAMBLE_LINES member. That over-generalized: several of those lines are
# short generics (`");"`, `"ctx.editor_target,"`, `"&*ctx.project,"`) that, as
# permanent standalone entries, mask ANY genuinely-deleted matching line in
# ALL future commits — e.g. a dropped match arm's call-closer `");"` would
# silently vanish from its residue signature instead of being caught. D-21
# fixes this: the drifted lines are kept as an ORDERED SEQUENCE, and
# classify() below matches them with a stateful REMOVAL-SIDE-only tracker —
# armed only by the exact opener, advanced only by the exact next line in
# order, disarmed (mismatch falls to residue) the instant a line breaks the
# chain. A `");"` (or any other member of this sequence) seen out of order or
# in isolation is no longer scaffold — it is caught as residue, same as any
# other genuinely deleted line.
PREAMBLE_LINES = {
    "let (effective_tab, effective_active_layer) = super::editor_dispatch_context"
    "(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), "
    "ctx.active_layer);",
    "let active_layer = &effective_active_layer;",
}
# The exact ordered drifted sequence (opener first, through the closer), one
# physical source line each — git emits each physical line of a removed
# multi-line statement as its own `-` line, which is why this is a sequence
# of lines rather than one joined statement like the canonical form above.
# REMOVAL-side only: when the LAST preamble-using domain moves out, the
# drifted original is deleted with nothing left behind to inherit it (the new
# location recomputes the CANONICAL form, matched above), so this sequence
# only ever needs to match `-` diff lines, never `+`.
DRIFTED_PREAMBLE_SEQUENCE = (
    "let (effective_tab, effective_active_layer) = super::editor_dispatch_context(",
    "ctx.editor_target,",
    "&*ctx.project,",
    "ctx.ui.inspector.last_effect_tab(),",
    "&*ctx.active_layer,",
    ");",
    "let active_layer: &Option<LayerId> = &effective_active_layer;",
)


def _normalize_ws(body: str) -> str:
    """Collapse internal whitespace to single spaces for exact-string comparison."""
    return " ".join(body.split())


def drop_visibility_pairs(residue: list[str]) -> tuple[list[str], int]:
    """Remove matched -old/+new pairs that are identical after stripping a
    leading visibility qualifier from the line's code (post +/- marker,
    whitespace-insensitive). Returns (remaining residue, pairs dropped)."""

    def key(line: str) -> str:
        body = line[1:].lstrip()
        return " ".join(VIS.sub("", body, count=1).split())

    minus: dict[str, int] = {}
    plus: dict[str, int] = {}
    for line in residue:
        (minus if line.startswith("-") else plus)[key(line)] = (
            (minus if line.startswith("-") else plus).get(key(line), 0) + 1
        )
    pairs = 0
    remaining: list[str] = []
    # Two passes so ordering inside the diff doesn't matter: count matches,
    # then emit unmatched lines in original order.
    matched: dict[str, int] = {}
    for k in minus:
        m = min(minus[k], plus.get(k, 0))
        if m:
            matched[k] = m
            pairs += m
    spent: dict[tuple[str, str], int] = {}
    for line in residue:
        k = key(line)
        side = "-" if line.startswith("-") else "+"
        if matched.get(k, 0) > spent.get((k, side), 0):
            spent[(k, side)] = spent.get((k, side), 0) + 1
            continue
        remaining.append(line)
    return remaining, pairs


# include_str! path-depth prefix rewrite (D6, Wave 3): moving a test mod DEEPER
# into a directory module (freeze/codegen.rs's `gpu_tests` -> freeze/codegen/
# gpu_tests.rs, one level; preset_runtime.rs's test mods -> preset_runtime/
# tests/*.rs, two levels) makes every relative `include_str!("../…")` argument
# resolve from a deeper directory, so its leading `../` run must grow by exactly
# the added nesting depth. That is the ONLY content change a test-mod relocation
# legitimately forces onto a moved line — every other byte of the line is
# preserved. This class pairs a removed line against an added line that are
# identical after collapsing the LEADING `../` run of every `include_str!`
# argument on the line (whitespace-insensitive); ANYTHING else different — the
# path tail, the surrounding code, a second literal — breaks the pair and both
# lines fall to residue. SMUGGLE-PROOF: a changed shader/asset path, or any code
# edit sharing the line, changes the normalized key and is caught. Only lines
# CONTAINING `include_str!` participate; every other residue line passes through
# untouched, so the class can never mask an unrelated deletion. Leading run
# only: a `../` appearing mid-path (not right after the opening quote) is part
# of the tail and is NOT collapsed — changing it is caught.
INCLUDE_STR_LEADING_DOTDOT = re.compile(r'(include_str!\s*\(\s*")(?:\.\./)+')


def drop_include_str_prefix_pairs(residue: list[str]) -> tuple[list[str], int]:
    """Remove matched -old/+new pairs of `include_str!` lines that are identical
    after collapsing the leading `../` run of every include_str! argument on the
    line (post +/- marker, whitespace-insensitive). Only lines containing
    `include_str!` participate. Returns (remaining residue, pairs dropped)."""

    def key(line: str) -> str:
        body = INCLUDE_STR_LEADING_DOTDOT.sub(r"\1", line[1:])
        return " ".join(body.split())

    minus: dict[str, int] = {}
    plus: dict[str, int] = {}
    for line in residue:
        if "include_str!" not in line:
            continue
        bucket = minus if line.startswith("-") else plus
        bucket[key(line)] = bucket.get(key(line), 0) + 1
    matched: dict[str, int] = {}
    pairs = 0
    for k in minus:
        m = min(minus[k], plus.get(k, 0))
        if m:
            matched[k] = m
            pairs += m
    remaining: list[str] = []
    spent: dict[tuple[str, str], int] = {}
    for line in residue:
        if "include_str!" not in line:
            remaining.append(line)
            continue
        k = key(line)
        side = "-" if line.startswith("-") else "+"
        if matched.get(k, 0) > spent.get((k, side), 0):
            spent[(k, side)] = spent.get((k, side), 0) + 1
            continue
        remaining.append(line)
    return remaining, pairs


def classify(out: str, claimed: dict[int, str] | None = None) -> tuple[dict[str, int], list[str]]:
    """Bucket every +/- line of a `--color-moved` diff into moved / allowlisted
    wiring / comment / dispatch-split scaffold, returning (counts, residue).
    Split out of `main` so the self-test can feed synthetic colored diffs
    without constructing a git repo."""
    residue: list[str] = []
    counts = {"moved": 0, "allowed": 0, "comments": 0, "scaffold": 0}
    counts.update(skeletons=0, manifests=0, rewrites=0)
    # Per-sign use-block state (D-18): True while a `-` (resp. `+`) multi-line
    # `use { ... }` opened by a SIGNED line is open and hasn't hit its
    # closing `};` yet.
    open_block = {"+": False, "-": False}
    # Context-opened use-block state (D-20 i): True while a multi-line
    # `use { ... }` whose OPENER is an UNCHANGED (context) line is open. This
    # is a single SHARED flag, not per-sign — the opener is unchanged so it
    # applies to both the old and new file, and governs +/- inner lines of
    # either sign until a context (unchanged) closer disarms it.
    context_block = False
    # Context-opened test-mod cfg state (W3-D4, the D-20(i) analog of
    # pending_test_attr): a CONTEXT (unchanged) `#[cfg(test)]`/feature-gated cfg
    # line arms the following signed `mod X {` opener as wiring. git keeps the
    # cfg line as context (not a signed self-move) when a RUN of consecutive
    # inline test mods is converted to `#[cfg] #[path="…"] mod X;` decls at once
    # (P3-R's e09e078b, 11/11): git's minimal diff anchors the identical cfg
    # lines as context and only diffs the mod lines, so pending_test_attr (which
    # only arms off a SIGNED cfg line) never fires. Shared, not per-sign — the
    # cfg is unchanged so it applies to both old and new. One-line lifetime: set
    # by a context cfg line, consumed by the immediately-following signed line.
    context_test_attr = False
    # Drifted-preamble removal-side sequence state (D-21): index of the NEXT
    # expected line in DRIFTED_PREAMBLE_SEQUENCE, or None while disarmed.
    # Removal-side only, so unlike open_block it is not per-sign. Reset
    # everywhere the other block trackers reset — a stray later `-");"`
    # elsewhere in the diff must never inherit an armed sequence.
    drifted_idx: int | None = None
    # Test-mod wiring state (D7a): pending_test_attr[sign] is True for exactly
    # the one line following a cfg-test attribute of that sign (only a `mod {`
    # opener there is wiring); test_mod_depth[sign] counts open test-mod braces
    # so a bare `}` closing one is wiring. Reset with the other trackers.
    pending_test_attr = {"+": False, "-": False}
    test_mod_depth = {"+": 0, "-": 0}
    for index, raw in enumerate(out.splitlines()):
        is_moved = bool(MOVED_RE.match(raw))
        plain = ANSI.sub("", raw)
        if HEADER.match(plain):
            # Diff file header (+++ / ---): never content, and a use block
            # can never legitimately span one.
            open_block["+"] = False
            open_block["-"] = False
            context_block = False
            context_test_attr = False
            drifted_idx = None
            pending_test_attr["+"] = False
            pending_test_attr["-"] = False
            test_mod_depth["+"] = 0
            test_mod_depth["-"] = 0
            continue
        if not plain.startswith(("+", "-")):
            # Hunk header (`@@ ... @@`) or real unchanged context line. A
            # signed block can't legitimately span either, so that state
            # always resets here.
            open_block["+"] = False
            open_block["-"] = False
            drifted_idx = None
            pending_test_attr["+"] = False
            pending_test_attr["-"] = False
            test_mod_depth["+"] = 0
            test_mod_depth["-"] = 0
            context_test_attr = False
            if plain.startswith("@"):
                # Hunk boundary: never content, and a context-opened block
                # can't legitimately span one either — two unrelated use
                # blocks in different hunks must never be treated as one.
                context_block = False
                continue
            # Real context line — it may be the opener or closer of a
            # context-opened block (D-20 i), so check before dropping it.
            body = plain[1:].strip() if plain else ""
            if context_block:
                if USE_CLOSE.match(body):
                    context_block = False
            elif USE_OPEN.match(body):
                context_block = True
            # A context `#[cfg(test)]`/feature-gated cfg line arms the following
            # signed `mod X {` opener (W3-D4); any other context line disarms it
            # (one-line lifetime, mirroring the signed pending_test_attr arm).
            context_test_attr = bool(CFG_TEST_ATTR.match(body))
            continue
        sign = plain[0]
        if sign == "+":
            # The drifted sequence is removal-side only: a `+` line can never
            # arm, advance, or belong to it, and it breaks any run in
            # progress (the removed lines a chain tracks are no longer
            # contiguous once an addition interrupts them).
            drifted_idx = None
        # Drifted-preamble removal-side sequence MATCH/ADVANCE (D-21, S5b
        # fix): computed BEFORE the is_moved short-circuit below, for every
        # removal-side line unconditionally. git's move detector works on
        # CONTENT identity alone — it flags a drifted removal line "moved"
        # whenever some other hunk happens to add an identical line
        # elsewhere (e.g. the short generic `");"` closer, which recurs
        # verbatim in code that moved to a sibling module — confirmed
        # against fb59db17's real diff). If the tracker were only consulted
        # after the is_moved continue (as it was pre-S5b), such a line would
        # be counted moved and skip the tracker entirely, leaving drifted_idx
        # one step behind for every subsequent line — desyncing the sequence
        # and surfacing a genuinely-dead later line as residue. Checking the
        # match here, ahead of is_moved, means a matching line ADVANCES the
        # tracker regardless of its moved-flag, keeping drifted_idx's
        # position a function of the CONTENT sequence of removal-side lines,
        # not of git's moved-flag.
        #
        # DISARM-on-mismatch deliberately stays OUT of this block (unlike the
        # pre-S5b single-site version) and is decided below instead, at the
        # original fallthrough site, AFTER open_block/ALLOW/COMMENT/
        # PREAMBLE_LINES/SCAFFOLD have all had a chance to claim the line.
        # Reason (also confirmed against fb59db17's real diff): the actual
        # drifted preamble in inspector.rs has two ordinary comment lines
        # ("// No arm mutates …") sitting between the sequence's `);` closer
        # and its final `let active_layer: …` line — comment lines that were
        # ALWAYS transparent to this tracker pre-S5b, because they are
        # consumed by the COMMENT check (below) and `continue` before ever
        # reaching the old single-site drifted check. Disarming here
        # unconditionally on every non-matching removal line (including
        # those comments) would break that transparency and reproduce a
        # residue-1 regression of its own. So: ADVANCE is unconditional and
        # happens here; DISARM only fires for a line that reaches the
        # bottom of the classification chain without being claimed by
        # anything else — exactly where it fired pre-S5b.
        #
        # ADVANCE is decoupled from CLASSIFICATION either way: this block
        # only updates drifted_idx and records whether this line matched
        # (drifted_match) — it does NOT touch counts. The line's bucket is
        # decided further down: a moved line stays moved (the is_moved
        # continue right here, untouched, no double-count); a non-moved
        # matched line becomes scaffold at the original site; a non-moved
        # unmatched line that reaches the bottom disarms the tracker there
        # and falls to residue — the smuggle-proof catch (a lone
        # out-of-sequence `");"` etc. is still residue) is unchanged.
        drifted_match = False
        if sign == "-":
            body = _normalize_ws(plain[1:])
            if drifted_idx is not None and body == DRIFTED_PREAMBLE_SEQUENCE[drifted_idx]:
                drifted_match = True
                drifted_idx += 1
                if drifted_idx == len(DRIFTED_PREAMBLE_SEQUENCE):
                    drifted_idx = None  # sequence complete: disarm
            elif body == DRIFTED_PREAMBLE_SEQUENCE[0]:
                # Either a fresh arm, or a mismatch that happens to be a new
                # opener — re-arm on it either way.
                drifted_match = True
                drifted_idx = 1
        # Test-mod cfg-attr ARM — BEFORE the is_moved short-circuit (W3-D2, the
        # same "advance ahead of is_moved" idiom the drifted-preamble tracker
        # above uses, and for the same reason). D7a arms `pending_test_attr` off
        # a `#[cfg(test)]`/feature-gated cfg attribute so the `mod X {` opener on
        # the immediately-following same-sign line is recognized as wiring. In a
        # 1-old→N-new test distribution the added cfg attrs have no removed
        # counterpart, so they are non-moved and the old post-is_moved arming
        # sufficed. But converting ONE inline `#[cfg(...)] mod X { … }` into a
        # `mod X;` declaration + sibling file (W3-D2: P3-C's tests.rs/gpu_tests,
        # P3-R's 11 #[path] tests-out decls) RE-ADDS the identical cfg line
        # 1-to-1, and git's --color-moved pairs it as a self-move — so it would
        # short-circuit as `moved` below before ever arming, and the following
        # `-mod X {` opener would fall to false residue (P3-C range: exactly the
        # two `-mod dispatch_contract_tests {` / `-mod gpu_tests {` lines).
        # Arming here fixes the 1-to-1 case and is a no-op for the 1-to-N case.
        # `was_test_attr` captures the PREVIOUS same-sign line's arm; the flag is
        # then re-derived from whether THIS line is itself a cfg-test attr. A
        # moved cfg line still arms the next line but is still bucketed as moved
        # below (no double count); the arm survives only to the immediately-next
        # same-sign line (any other line, moved or not, re-derives it) — so a
        # non-`mod {` line after a cfg attr can never be smuggled in as wiring.
        tm_body = plain[1:].strip()
        # Consume either arm: a same-sign SIGNED cfg attr (pending_test_attr) or
        # a shared CONTEXT cfg attr (context_test_attr, W3-D4). Both live only to
        # this immediately-following signed line; clear the context arm now.
        was_test_attr = pending_test_attr[sign] or context_test_attr
        context_test_attr = False
        pending_test_attr[sign] = bool(CFG_TEST_ATTR.match(tm_body))
        # Use-block opener ARM — BEFORE the is_moved short-circuit (W3-D3, the
        # use-block sibling of the test-mod fix above; same root cause). D-18
        # arms `open_block[sign]` off a `use …::{` opener so the block's item
        # continuation lines are recognized as wiring — but that arming lives in
        # the ALLOW branch BELOW the is_moved short-circuit. When a directory
        # split redistributes one combined multi-line `use path::{ … }` import
        # across several sibling modules, the identical `use path::{` opener text
        # recurs on both the removed (1×) and added (N×) sides, so git's
        # --color-moved=plain flags every opener as a MOVE — it short-circuits as
        # `moved` before ALLOW ever arms the block, and the RE-GROUPED item lines
        # (regrouped across physical lines, so none move-pair) fall to false
        # residue (P3-G: the manifold_core::effect_graph_def redistribution).
        # Arming here, ahead of is_moved, opens the block regardless of the
        # opener's moved-flag. Smuggle-proofing unchanged: only USE_ITEM-shaped
        # lines inside an open block are waived (a real statement still falls to
        # residue), and a hunk/context boundary still resets the block.
        if USE_OPEN.match(tm_body):
            open_block[sign] = True
        if claimed and index in claimed:
            if claimed[index] != "blocked":
                counts[claimed[index]] += 1
            continue
        if is_moved:
            counts["moved"] += 1
            continue
        # Test-mod wiring header shapes (D7a; non-moved lines only — moved test
        # bodies short-circuited above, so their internal braces never reach the
        # depth counter). The cfg attr itself is wiring; a `mod X {` opener right
        # after one opens a counted brace; a bare `}` closing a counted brace is
        # wiring. Any other line under the class falls through to residue
        # (smuggle-proof). `pending_test_attr` was already armed above.
        if CFG_TEST_ATTR.match(tm_body):
            counts["allowed"] += 1
            continue
        if was_test_attr and MOD_OPEN.match(tm_body):
            counts["allowed"] += 1
            test_mod_depth[sign] += 1
            continue
        if test_mod_depth[sign] > 0 and BARE_CLOSE.match(tm_body):
            counts["allowed"] += 1
            test_mod_depth[sign] -= 1
            continue
        if open_block[sign] or context_block:
            body = plain[1:].strip()
            if USE_ITEM.match(body):
                counts["allowed"] += 1
                if "}" in body:
                    open_block[sign] = False
                    context_block = False
                continue
            # Not import-item-shaped: a real line was smuggled inside the
            # open block (signed or context-opened alike). Leave the block
            # "open" (a well-formed diff will still close it later) and fall
            # through to residue below.
        if ALLOW.match(plain):
            counts["allowed"] += 1
            if USE_OPEN.match(plain[1:].strip()):
                open_block[sign] = True
            continue
        if COMMENT.match(plain):
            counts["comments"] += 1
            continue
        if _normalize_ws(plain[1:]) in PREAMBLE_LINES:
            counts["scaffold"] += 1
            continue
        if SCAFFOLD.match(plain):
            counts["scaffold"] += 1
            continue
        if sign == "-":
            # Reaching here means this line is NOT moved, and none of
            # open_block/ALLOW/COMMENT/PREAMBLE_LINES/SCAFFOLD claimed it —
            # the original fallthrough site (D-21), now driven by the
            # match already computed above (S5b) rather than re-deriving it.
            if drifted_match:
                counts["scaffold"] += 1
                continue
            # DISARM here, not in the pre-is_moved block above: this is the
            # instant a removal-side line breaks the chain FOR REAL (it
            # wasn't absorbed as comment/wiring/scaffold either), so the next
            # line must not inherit a stale armed sequence — exactly the
            # pre-S5b disarm-on-mismatch behavior, just relocated to keep
            # comment/wiring/scaffold lines transparent to the tracker (see
            # the comment above).
            drifted_idx = None
        residue.append(plain)
    return counts, residue


def rewrite_map(value: str) -> tuple[str, str]:
    old, sep, new = value.partition("=")
    path = r"\$?(?:r#)?[A-Za-z_]\w*(?:::(?:r#)?[A-Za-z_]\w*)*(?:!::|::)"
    if not sep or not re.fullmatch(path, old) or not re.fullmatch(path, new):
        raise argparse.ArgumentTypeError("rewrite must be OLD::=NEW:: Rust path prefixes")
    return old, new


# Keep literals opaque: path-looking text in a shader, string or comment is not
# a Rust path. The scanner also supplies inline-module depth for super:: paths.
RUST_PATH = r'\$?(?:r#)?[A-Za-z_]\w*(?:::(?:r#)?[A-Za-z_]\w*)*(?:::)?'
RUST_TOKEN = re.compile(
    r'//[^\n]*|/\*|(?:br|r)(?P<hashes>\#*)"[\s\S]*?"(?P=hashes)'
    r'|b?"(?:\\[\s\S]|[^"\\])*"|b?\'(?:\\.|[^\'\\\n])\''
    rf'|::|(?<![\w$]){RUST_PATH}|[^\s]', re.MULTILINE)


def rust_tokens(source):
    end = 0
    while match := RUST_TOKEN.search(source, end):
        end = match.end()
        token = match[0]
        if token == '/*':
            depth = 1
            while depth and end < len(source):
                if source[end:end + 2] in ('/*', '*/'):
                    depth += 1 if source[end:end + 2] == '/*' else -1
                    end += 2
                else:
                    end += 1
            yield match.start(), source[match.start():end], False
        else:
            yield match.start(), token, bool(re.fullmatch(RUST_PATH, token))


def macro_ranges(source, recorded=()):
    """Macro payloads are text unless their macro has a recorded map."""
    tokens = list(rust_tokens(source))
    ranges = []
    for i, (_, token, _) in enumerate(tokens):
        if token != '!' or i + 1 >= len(tokens) or not i or not tokens[i - 1][2]:
            continue
        opening = i + 1
        if i and tokens[i - 1][1] == 'macro_rules':
            opening += 1
        if opening >= len(tokens) or tokens[opening][1] not in ('(', '[', '{'):
            continue
        name = tokens[i - 1][1] if i else ''
        stack = []
        for end in range(opening, len(tokens)):
            text = tokens[end][1]
            if text in ('(', '[', '{'):
                stack.append({'(': ')', '[': ']', '{': '}'}[text])
            elif stack and text == stack[-1]:
                stack.pop()
                if not stack:
                    ranges.append((tokens[opening][0], tokens[end][0] + 1,
                                   name in recorded))
                    break
    return ranges


def opaque_macro_offsets(source, recorded=()):
    return [(a, b) for a, b, permitted in macro_ranges(source, recorded) if not permitted]


def build_source_spans(source):
    """Only literal source-list arguments and literal Cargo rerun directives."""
    tokens = list(rust_tokens(source))
    # A locally declared consumer has no verified source-list contract.
    local_consumer = any(token in ('mod', 'as') and tokens[i + 1][1] == 'native_source_identity'
                         for i, (_, token, _) in enumerate(tokens[:-1]))
    opaque = opaque_macro_offsets(source)
    spans = []
    for i, (start, token, _) in enumerate(tokens):
        if local_consumer or token != 'native_source_identity::emit_source_identity' or any(a < start < b for a, b in opaque):
            continue
        if i + 1 >= len(tokens) or tokens[i + 1][1] != '(':
            continue
        depth, argument, values = [], 0, []
        for pos, text, _ in tokens[i + 2:]:
            if not depth and text in (',', ')'):
                if argument == 1:
                    # Refuse expressions, identifiers and computed strings.
                    if (len(values) >= 3 and [v for _, v in values[:2]] == ['&', '[']
                            and values[-1][1] == ']'
                            and all(v == ',' or re.fullmatch(r'"[^"\\\n]+"', v)
                                    for _, v in values[2:-1])):
                        spans.extend((p + 1, p + len(v) - 1) for p, v in values[2:-1] if v != ',')
                    break
                argument += 1
                values = []
                if text == ')':
                    break
                continue
            values.append((pos, text))
            if text in ('(', '[', '{'):
                depth.append({'(': ')', '[': ']', '{': '}'}[text])
            elif depth and text == depth[-1]:
                depth.pop()
    starts = {p for p, t, _ in tokens if t == 'println'}
    for match in re.finditer(r'\bprintln!\s*\(\s*"cargo::?rerun-if-changed=([^"\\\n]+)"\s*\)', source):
        if match.start() in starts:
            spans.append(match.span(1))
    return spans


def text_identity_regions(source, recorded=(), source_spans=()):
    """Literal and opaque-macro byte identity overrides line-based waivers."""
    regions = [(a, b) for a, b in opaque_macro_offsets(source, recorded)
               if not any(a <= x < y <= b for x, y in source_spans)]
    for start, token, is_path in rust_tokens(source):
        if not is_path and not token.startswith(('//', '/*')) and (
                '"' in token or (token.startswith("'") and len(token) > 1)):
            if not any(start <= a < b <= start + len(token) for a, b in source_spans):
                regions.append((start, start + len(token)))
    return regions


def changed_region_lines(before, after, old_regions, new_regions, inventories=None):
    old_values = Counter(before[a:b] for a, b in old_regions)
    new_values = Counter(after[a:b] for a, b in new_regions)
    result = {'-': set(), '+': set()}
    # Extraction can remove payloads, but surviving payloads must retain their
    # occurrence order. Membership elsewhere cannot authorize a swap.
    exchanged = set()
    shared = old_values.keys() & new_values.keys()
    old_order = [before[a:b] for a, b in old_regions if before[a:b] in shared]
    new_order = [after[a:b] for a, b in new_regions if after[a:b] in shared]
    if old_order != new_order:
        exchanged.update(shared)
    for sign, source, regions, own, other in (
            ('-', before, old_regions, old_values, new_values),
            ('+', after, new_regions, new_values, old_values)):
        for a, b in regions:
            if source[a:b] in exchanged or (source[a:b] not in inventories['+' if sign == '-' else '-'] if inventories is not None
                    else own[source[a:b]] != other[source[a:b]]):
                result[sign].update(range(source.count('\n', 0, a) + 1,
                                          source.count('\n', 0, b) + 2))
    return result


def item_attributes(source):
    """Associate attributes with their following item, never just a loose line."""
    tokens = list(rust_tokens(source))
    opaque = opaque_macro_offsets(source)
    records, regions = [], []
    i = 0
    while i < len(tokens):
        start, token, _ = tokens[i]
        if token != '#' or any(a <= start < b for a, b in opaque):
            i += 1
            continue
        first = i
        attrs = []
        while i + 1 < len(tokens) and tokens[i][1] == '#':
            i += 1
            if tokens[i][1] == '!':
                i += 1
            if tokens[i][1] != '[':
                break
            depth = 0
            group = []
            while i < len(tokens):
                value = tokens[i][1]
                group.append(value)
                depth += value == '['
                depth -= value == ']'
                i += 1
                if depth == 0:
                    break
            attrs.extend(group)
        header = []
        end = tokens[i - 1][0] + len(tokens[i - 1][1])
        for _, value, _ in tokens[i:]:
            if value in ('{', ';', '='):
                break
            header.append(value)
        records.append((tuple(attrs), tuple(header)))
        regions.append((tokens[first][0], end))
        if i == first:
            i += 1
    return records, regions


def crate_context(path, reader):
    match = re.match(r'(crates/[^/]+)/(.*)', path or '')
    if not match:
        return None, None, ()
    root, relative = match.groups()
    manifest = tomllib.loads(reader(root + '/Cargo.toml') or '')
    name = manifest.get('lib', {}).get('name') or manifest.get('package', {}).get('name')
    if not name:
        return root, None, ()
    parts = relative.removeprefix('src/').removesuffix('.rs').split('/')
    if parts[-1] in ('lib', 'main', 'mod'):
        parts.pop()
    # File-layout context is not proof for an explicitly mounted module. Keep
    # absolute crate paths usable, but decline relative-path claims there.
    candidates = {root + '/src/lib.rs', root + '/src/main.rs'}
    directory = posixpath.dirname(path)
    while directory.startswith(root + '/src'):
        candidates.update((directory + '.rs', directory + '/mod.rs'))
        directory = posixpath.dirname(directory)
    for parent in candidates - {path}:
        for mount in re.finditer(r'#\[path\s*=\s*"([^"]+)"\]', reader(parent) or ''):
            mounted = posixpath.normpath(posixpath.join(posixpath.dirname(parent), mount[1]))
            if mounted == path:
                return root, name.replace('-', '_'), None
    return root, name.replace('-', '_'), tuple(parts)


def rename_pairs(blocks):
    result = {}
    for lines in blocks:
        plain = [ANSI.sub('', line) for line in lines]
        old = next((s[12:] for s in plain if s.startswith('rename from ')), None)
        new = next((s[10:] for s in plain if s.startswith('rename to ')), None)
        if old and new:
            result[old] = new
    return result


def lockfile_lines(source, before, read_old, read_new, donor_paths):
    """Whole package validation; never waive individual dependency/name lines."""
    old_packages = {p['name']: p for p in tomllib.loads(before).get('package', [])
                    if 'source' not in p}
    created = {}
    for root, donors in donor_paths.items():
        manifest = tomllib.loads(read_new(root + '/Cargo.toml'))
        package = manifest.get('package', {})
        name = package.get('name')
        if name and name not in old_packages:
            created[name] = (root, manifest, donors)

    def dependencies(manifest):
        result = set()
        for scope in [manifest, *manifest.get('target', {}).values()]:
            for kind in ('dependencies', 'dev-dependencies', 'build-dependencies'):
                for key, value in scope.get(kind, {}).items():
                    result.add(value.get('package', key) if isinstance(value, dict) else key)
        return result

    valid_created = set()
    packages = tomllib.loads(source).get('package', [])
    for package in packages:
        name = package['name']
        if name not in created or set(package) != {'name', 'version', 'dependencies'}:
            continue
        if sum(entry['name'] == name for entry in packages) != 1:
            continue
        _, manifest, donors = created[name]
        inherited = set()
        for donor in donors:
            donor_name = tomllib.loads(read_old(donor)).get('package', {}).get('name')
            inherited.update(old_packages.get(donor_name, {}).get('dependencies', []))
        deps = package['dependencies']
        if (package['version'] == manifest['package'].get('version')
                and len(set(deps)) == len(deps) and set(deps) <= inherited
                and {dep.split()[0] for dep in deps} == dependencies(manifest)):
            valid_created.add(name)
    allowed = set()
    headers = [m.start() for m in re.finditer(r'^\[\[package\]\]$', source, re.MULTILINE)]
    for start, end in zip(headers, headers[1:] + [len(source)]):
        package = tomllib.loads(source[start:end])['package'][0]
        name = package['name']
        ok = name in valid_created
        old = old_packages.get(name)
        if old and package != old:
            old_rest = {k: v for k, v in old.items() if k != 'dependencies'}
            new_rest = {k: v for k, v in package.items() if k != 'dependencies'}
            old_deps, new_deps = old.get('dependencies', []), package.get('dependencies', [])
            additions = set(new_deps) - set(old_deps)
            # Existing packages may only gain edges to verified new packages.
            ok = (old_rest == new_rest and set(old_deps) <= set(new_deps)
                  and bool(additions) and additions <= valid_created
                  and len(set(new_deps)) == len(new_deps))
            manifest = tomllib.loads(read_new('crates/' + name + '/Cargo.toml') or '')
            ok = ok and additions <= dependencies(manifest)
        if ok:
            allowed.update(range(source.count('\n', 0, start) + 1,
                                 source.count('\n', 0, end) + 1))
    return allowed


def rust_path_resolver(path, reader, rewrites, other_path=None, other_reader=None, renames=None):
    """One resolver shared by exact lines and expanded import leaves."""
    root, crate, module = crate_context(path, reader)
    _, new_crate, _ = crate_context(other_path, other_reader) if other_reader else (None, None, None)
    mapping = {old.removesuffix('::'): new.removesuffix('::') for old, new in rewrites}
    if crate:
        mapping = {(crate + key[len('crate'):] if key.startswith('crate::') else
                    crate + key[len('$crate'):] if key.startswith('$crate::') else key): value
                   for key, value in mapping.items()}
    if new_crate:
        mapping = {key: (new_crate + value[len('crate'):] if value.startswith('crate::')
                         else '$' + new_crate + value[len('$crate'):] if value.startswith('$crate::')
                         else value) for key, value in mapping.items()}
    prefixes = sorted(mapping, key=len, reverse=True)
    local_source = reader(path) or ''
    opaque = opaque_macro_offsets(local_source)
    local_tokens = [(pos, token) for pos, token, is_path in rust_tokens(local_source)
                    if is_path and not any(a <= pos < b for a, b in opaque)]
    local_names = {local_tokens[i + 1][1] for i, (_, token) in enumerate(local_tokens[:-1])
                   if token in ('mod', 'as')}
    def resolve(token, inline=(), macro=False):
        mapped = False
        absolute = token.startswith('::')
        token = token.removeprefix('::')
        trailing = '::' if token.endswith('::') else ''
        value = token.removesuffix('::')
        # Explicit local maps retain their old context-free meaning when
        # no manifest exists (historical god-file fixtures).
        canonical = value
        parts = value.split('::')
        if crate and parts[0] in ('crate', '$crate', 'self', 'super'):
            if module is None and parts[0] in ('self', 'super'):
                return token, False
            scope = list(module or ()) + list(inline)
            head = parts.pop(0)
            if head in ('crate', '$crate'):
                scope = []
            elif head == 'super':
                if not scope:
                    return token, False
                scope.pop()
                while parts and parts[0] == 'super':
                    if not scope:
                        break
                    scope.pop()
                    parts.pop(0)
            canonical = '::'.join([crate, *scope, *parts])
        elif crate and not absolute and parts[0] in local_names:
            canonical = '::'.join([crate, *list(module or ()), *inline, *parts])
        original_canonical = canonical
        macro_key = canonical + '!' if macro else None
        if macro_key in mapping:
            canonical = mapping[macro_key].removesuffix('!')
            mapped = True
        else:
            key = next((key for key in prefixes if canonical == key or canonical.startswith(key + '::')), None)
            if key is not None:
                canonical = mapping[key] + canonical[len(key):]
                mapped = True
        if mapped and value.endswith('::*'):
            # A declared prefix map alone says nothing about a glob's exports.
            # Require the module source itself to be a recorded rename.
            old_module = original_canonical.removesuffix('::*')
            new_module = canonical.removesuffix('::*')
            moved_modules = set()
            for old_file, new_file in (renames or {}).items():
                if not old_file.endswith('.rs') or not new_file.endswith('.rs'):
                    continue
                _, old_name, old_parts = crate_context(old_file, reader)
                _, new_name, new_parts = crate_context(new_file, other_reader)
                if old_name and new_name and old_parts is not None and new_parts is not None:
                    moved_modules.add(('::'.join((old_name, *old_parts)),
                                       '::'.join((new_name, *new_parts))))
            same_module = any(
                old_module == a and new_module == b
                or (old_module.startswith(a + '::') and new_module.startswith(b + '::')
                    and old_module[len(a):] == new_module[len(b):])
                for a, b in moved_modules)
            if old_module != new_module and not same_module:
                return '!unproved-glob:' + original_canonical, False
        if crate and token.startswith('$crate::') and not canonical.startswith('$') and not any(
                old.startswith('$crate::') and token.startswith(old) for old, _ in rewrites):
            canonical = '$' + canonical
        return ('::' if absolute else '') + canonical + trailing, mapped

    return resolve


def recorded_macro_names(source, resolve, rewrites):
    names = {p.removesuffix('!::') for pair in rewrites for p in pair if p.endswith('!::')}
    tokens = list(rust_tokens(source))
    for i, (_, token, is_path) in enumerate(tokens[:-1]):
        if is_path and tokens[i + 1][1] == '!':
            canonical, _ = resolve(token, macro=True)
            if canonical in names:
                names.add(token)
    return names


def contextual_lines(source, path, reader, rewrites, renames, other_path=None,
                     other_reader=None, proved=None, recorded_macros=None):
    """Canonical line keys, preserving every non-path token and literal byte.

    Only the old side receives declared rewrites. Canonical crate/module paths
    let the new side spell that *same* target as crate, $crate or super. Renames
    supply include ownership; no basename or equal-content search is used.
    """
    root, _, _ = crate_context(path, reader)
    resolve = rust_path_resolver(path, reader, rewrites, other_path, other_reader, renames)
    stack, inline, pending = [], [], None
    replacements = []
    code_starts = set()
    literal_lines = set()
    recorded = (recorded_macros if recorded_macros is not None else
                recorded_macro_names(source, resolve, rewrites))
    opaque = opaque_macro_offsets(source, recorded)
    for start, token, is_path in rust_tokens(source):
        if '\n' in token and not token.startswith(('//', '/*')):
            first = source.count('\n', 0, start)
            literal_lines.update(range(first + 1, first + token.count('\n') + 1))
        if any(a < start < b for a, b in opaque):
            continue
        if is_path:
            code_starts.add(start)
        if token == 'mod':
            pending = ''
        elif pending == '' and is_path:
            pending = token
        elif token == '{':
            stack.append(pending)
            if pending:
                inline.append(pending)
            pending = None
        elif token == '}':
            if stack and stack.pop():
                inline.pop()
            pending = None
        elif token == ';':
            pending = None
        if is_path and ('::' in token):
            absolute = source[:start].endswith('::')
            canonical, mapped = resolve(('::' if absolute else '') + token, inline,
                                        source[start + len(token):start + len(token) + 1] == '!')
            if absolute:
                canonical = canonical.removeprefix('::')
            if mapped and proved is not None:
                proved.add(source.count('\n', 0, start) + 1)
            replacements.append((start, start + len(token), canonical))
    # Includes are source-relative, even in inline modules. Require the exact
    # file move, or the same existing file, and preserve all surrounding bytes.
    if other_path and other_reader:
        for match in re.finditer(r'\binclude_(?:str|bytes)!\s*\(\s*"([^"\n]+)"\s*\)', source):
            if match.start() not in code_starts:
                continue
            old_asset = posixpath.normpath(posixpath.join(posixpath.dirname(path), match[1]))
            new_asset = renames.get(old_asset, old_asset)
            old_bytes = reader(old_asset, raw=True)
            if old_bytes is not None and old_bytes == other_reader(new_asset, raw=True):
                target = posixpath.relpath(new_asset, posixpath.dirname(other_path))
                replacements.append((match.start(1), match.end(1), target))
                if proved is not None and target != match[1]:
                    proved.add(source.count('\n', 0, match.start()) + 1)
        if root and path == root + '/build.rs':
            new_root, _, _ = crate_context(other_path, other_reader)
            for start, end in build_source_spans(source):
                old_source = posixpath.normpath(root + '/' + source[start:end])
                if new_root and old_source in renames:
                    target = posixpath.relpath(renames[old_source], new_root)
                    replacements.append((start, end, target))
                    if proved is not None:
                        proved.add(source.count('\n', 0, start) + 1)
    pieces, end = [], 0
    for start, stop, value in sorted(replacements):
        pieces.extend((source[end:start], value))
        end = stop
    pieces.append(source[end:])
    return [line if number in literal_lines else line.strip()
            for number, line in enumerate(''.join(pieces).splitlines())]


def import_tree_leaves(tokens):
    """Parse only Rust use-tree grammar; consume every token or refuse proof."""
    parts = []
    for token in tokens:
        parts.extend(part for part in re.split(r'(::)', token) if part)
    cursor = 0
    leaves = []

    def tree(prefix):
        nonlocal cursor
        path = list(prefix)
        if cursor < len(parts) and parts[cursor] == '::':
            if prefix:
                raise ValueError('absolute path inside a prefixed group')
            path.append('')
            cursor += 1
        while cursor < len(parts):
            token = parts[cursor]
            if token == '{':
                cursor += 1
                while cursor < len(parts) and parts[cursor] != '}':
                    tree(path)
                    if cursor < len(parts) and parts[cursor] == ',':
                        cursor += 1
                    elif cursor >= len(parts) or parts[cursor] != '}':
                        raise ValueError('expected use-tree comma')
                if cursor >= len(parts):
                    raise ValueError('unclosed use tree')
                cursor += 1
                return
            if token == '*' or re.fullmatch(r'\$?(?:r#)?[A-Za-z_]\w*', token):
                path.append(token)
                cursor += 1
            else:
                raise ValueError('invalid use-tree item')
            if token != '*' and cursor < len(parts) and parts[cursor] == '::':
                cursor += 1
                continue
            alias = None
            if cursor < len(parts) and parts[cursor] == 'as':
                cursor += 1
                if token == '*' or cursor >= len(parts) or not re.fullmatch(r'(?:r#)?[A-Za-z_]\w*', parts[cursor]):
                    raise ValueError('invalid use alias')
                alias = parts[cursor]
                cursor += 1
            if len(path) > 1 and path[-1] == 'self':
                path.pop()
            leaves.append(('::'.join(path), alias))
            return
        raise ValueError('missing use-tree item')

    tree([])
    if cursor != len(parts):
        raise ValueError('non-use tokens in declaration')
    return leaves


def import_identity(source, resolve):
    """Return a scoped leaf multiset, declaration lines and non-use remainder.

    A scope ordinal ignores use trees, so regrouping/reordering imports cannot
    change it, but moving an import between functions cannot pass. Attributes
    and visibility are token tuples; literal contents and attribute order are
    preserved. Parse failure refuses the entire file's import proof.
    """
    opaque = opaque_macro_offsets(source)
    tokens = [(pos, token) for pos, token, _ in rust_tokens(source)
              if not any(a <= pos < b for a, b in opaque)
              and (not token.startswith(('//', '/*')) or token.startswith(('///', '/**')))]
    leaves, spans = Counter(), []
    valid = True
    i = 0
    prefix_start, attributes, visibility = None, [], ()
    scope, children, modules, inline = [], [0], [], []
    pending_module = None
    enclosing_attributes, item_attributes = [], []
    file_attributes = []

    def balanced(index, opener, closer):
        depth = 0
        for end in range(index, len(tokens)):
            text = tokens[end][1]
            depth += text == opener
            depth -= text == closer
            if depth == 0:
                return end + 1
        raise ValueError('unclosed attribute/visibility')

    while i < len(tokens):
        pos, token = tokens[i]
        if token.startswith(('///', '/**')):
            prefix_start = pos if prefix_start is None else prefix_start
            attributes.append((token,))
            i += 1
            continue
        inner = token == '#' and i + 2 < len(tokens) and tokens[i + 1][1] == '!'
        attr_open = i + 2 if inner else i + 1
        if token == '#' and attr_open < len(tokens) and tokens[attr_open][1] == '[':
            try:
                end = balanced(attr_open, '[', ']')
            except ValueError:
                valid = False
                break
            attribute = tuple(text for _, text in tokens[i:end])
            if inner:
                if enclosing_attributes:
                    enclosing_attributes[-1] += (attribute,)
                else:
                    file_attributes.append(attribute)
            else:
                prefix_start = pos if prefix_start is None else prefix_start
                attributes.append(attribute)
            i = end
            continue
        if token == 'pub':
            if visibility:
                valid = False
            end = i + 1
            if end < len(tokens) and tokens[end][1] == '(':
                try:
                    end = balanced(end, '(', ')')
                except ValueError:
                    valid = False
                    break
            prefix_start = pos if prefix_start is None else prefix_start
            visibility = tuple(text for _, text in tokens[i:end])
            i = end
            continue
        if token == 'use':
            end = next((j for j in range(i + 1, len(tokens)) if tokens[j][1] == ';'), None)
            if end is None:
                spans.append((prefix_start if prefix_start is not None else pos, len(source)))
                valid = False
                break
            spans.append((prefix_start if prefix_start is not None else pos, tokens[end][0] + 1))
            try:
                for full_path, alias in import_tree_leaves([text for _, text in tokens[i + 1:end]]):
                    canonical, _ = resolve(full_path, inline)
                    inherited = tuple(file_attributes) + tuple(attr for group in enclosing_attributes for attr in group)
                    leaves[(tuple(scope), visibility, inherited + tuple(attributes), canonical, alias)] += 1
            except ValueError:
                valid = False
            i = end + 1
            prefix_start, attributes, visibility = None, [], ()
            continue
        if attributes:
            item_attributes.extend(attributes)
        prefix_start, attributes, visibility = None, [], ()
        if token == 'mod':
            pending_module = ''
        elif pending_module == '' and re.fullmatch(r'\w+', token):
            pending_module = token
        elif token == '{':
            enclosing_attributes.append(tuple(item_attributes))
            item_attributes = []
            scope.append(children[-1])
            children[-1] += 1
            children.append(0)
            modules.append(pending_module)
            if pending_module:
                inline.append(pending_module)
            pending_module = None
        elif token == '}':
            if scope:
                scope.pop()
                children.pop()
                enclosing_attributes.pop()
                if modules.pop():
                    inline.pop()
            pending_module = None
        elif token == ';':
            pending_module = None
            item_attributes = []
        i += 1
    owned, pieces, end = set(), [], 0
    for start, stop in spans:
        owned.update(range(source.count('\n', 0, start) + 1, source.count('\n', 0, stop) + 2))
        pieces.extend((source[end:start], '\n' * source.count('\n', start, stop)))
        end = stop
    pieces.append(source[end:])
    return leaves if valid else None, owned, ''.join(pieces)


def skeleton_lines(source: str) -> bool:
    """Whole-file check; a declaration cannot hide a body after its semicolon."""
    in_use = False
    for line in source.splitlines():
        body = line.strip()
        if not body or body.startswith("//!"):
            continue
        if in_use and USE_ITEM.fullmatch(body):
            in_use = "}" not in body
            continue
        if USE_OPEN.fullmatch(body):
            in_use = True
            continue
        if re.fullmatch(r"(?:pub(?:\((?:crate|super)\))?\s+)?mod\s+\w+;", body):
            continue
        if re.fullmatch(r"(?:pub(?:\((?:crate|super)\))?\s+)?use\s+[\w:$*,{}\s]+;", body):
            continue
        if re.fullmatch(r"(?:#!\[[^\]\n]+\]|#\[cfg\([^\]\n]+\)\])", body):
            continue
        return False
    return not in_use


def workspace_member(directory: str, root: dict) -> bool:
    return (any(fnmatch.fnmatchcase(directory, member) for member in root.get("members", []))
            and not any(fnmatch.fnmatchcase(directory, member) for member in root.get("exclude", [])))


def external_dependency_values(paths, read_file) -> dict[tuple, list]:
    """Parsed external dependency precedents in the old workspace."""
    root = tomllib.loads(read_file("Cargo.toml") or "").get("workspace", {})
    result = {}
    for path in paths:
        if path != "Cargo.toml" and not workspace_member(posixpath.dirname(path), root):
            continue
        document = tomllib.loads(read_file(path))
        scopes = [(None, document), (None, document.get("workspace", {}))]
        scopes.extend(document.get("target", {}).items())
        for target, scope in scopes:
            for table in ("dependencies", "dev-dependencies", "build-dependencies"):
                for name, value in scope.get(table, {}).items():
                    if isinstance(value, str) or (isinstance(value, dict)
                                                  and not {"path", "workspace"} & value.keys()):
                        result.setdefault((target, table, name), []).append(value)
    return result


def bin_identity(value):
    """Only the bounded, crate-relative bin declaration can transfer."""
    if not isinstance(value, dict) or not set(value) <= {"name", "path", "required-features"}:
        return None
    name = value.get("name")
    if not isinstance(name, str) or not name:
        return None
    path = value.get("path", f"src/bin/{name}.rs")
    if not isinstance(path, str) or posixpath.isabs(path):
        return None
    path = posixpath.normpath(path)
    if path == ".." or path.startswith("../"):
        return None
    return name, path, value.get("required-features")


def transferred_bins(blocks, read_old, read_new) -> dict[tuple[str, str], set[int]]:
    """Pair declarations across manifests only when their sources also transfer."""
    removed, added = [], []
    renames = set()
    for lines in blocks:
        plain = [ANSI.sub("", line) for line in lines]
        rename_from = next((line.removeprefix("rename from ") for line in plain
                            if line.startswith("rename from ")), None)
        rename_to = next((line.removeprefix("rename to ") for line in plain
                          if line.startswith("rename to ")), None)
        if rename_from is not None and rename_to is not None:
            renames.add((rename_from, rename_to))
        old = next((line[4:].removeprefix("a/") for line in plain if line.startswith("--- ")), None)
        new = next((line[4:].removeprefix("b/") for line in plain if line.startswith("+++ ")), None)
        if not ((old and old.endswith("Cargo.toml")) or (new and new.endswith("Cargo.toml"))):
            continue
        before = tomllib.loads(read_old(old) or "") if old != "/dev/null" else {}
        after = tomllib.loads(read_new(new) or "") if new != "/dev/null" else {}
        surviving_names = {value.get("name") for value in after.get("bin", [])}
        for index, value in enumerate(before.get("bin", [])):
            identity = bin_identity(value)
            if identity is not None and value["name"] not in surviving_names:
                removed.append((old, index, identity))
        previous_names = {value.get("name") for value in before.get("bin", [])}
        if new and re.fullmatch(r"crates/[^/]+/Cargo.toml", new):
            for index, value in enumerate(after.get("bin", [])):
                if value.get("name") not in previous_names:
                    added.append((new, index, bin_identity(value)))
    result = {}
    for path, index, identity in added:
        if identity is None:
            continue
        for candidate, (old, old_index, old_identity) in enumerate(removed):
            if old != path and identity == old_identity:
                old_source = posixpath.join(posixpath.dirname(old), identity[1])
                new_source = posixpath.join(posixpath.dirname(path), identity[1])
                if (old_source, new_source) not in renames:
                    before = read_old(old_source, raw=True)
                    if before is None or before != read_new(new_source, raw=True):
                        continue
                result.setdefault(("+", path), set()).add(index)
                result.setdefault(("-", old), set()).add(old_index)
                removed.pop(candidate)
                break
    return result


def manifest_wiring(source: str, path: str, read_file, read_other, sign: str,
                    new_crate=False, external_values=None, other_path=None,
                    bins=frozenset(), donors=(), feature_transfers=()) -> tuple[set[int], set[int]]:
    """Eligible physical lines, with table context from the complete revision.

    Only added dev-dependencies may select features. Removing feature selectors
    is wiring only when the dependency key disappears from all dependency tables.
    New manifests may declare empty non-default features. Exact workspace lint
    inheritance is wiring in any manifest. Explicit
    rejections cannot pass through git move detection or legacy visibility pairs.
    """
    root = tomllib.loads(read_file("Cargo.toml") or "").get("workspace", {})
    other_root = tomllib.loads(read_other("Cargo.toml") or "").get("workspace", {})
    document = tomllib.loads(source)
    other_document = tomllib.loads(read_other(other_path or path) or "")

    def workspace_crate(directory: str) -> bool:
        directory = posixpath.normpath(directory)
        return (any(fnmatch.fnmatchcase(directory, member) for member in root.get("members", []))
                and not any(fnmatch.fnmatchcase(directory, member) for member in root.get("exclude", []))
                and bool(read_file(directory + "/Cargo.toml")))

    dependencies = {}
    surviving_dependencies = set()
    for table in ("dependencies", "dev-dependencies", "build-dependencies"):
        dependencies.update(document.get(table, {}))
        surviving_dependencies.update(other_document.get(table, {}))

    def local_dependency(value, conservative=True, dev_features=False) -> bool:
        keys = {"path", "package"}
        if not conservative:
            keys |= {"features", "optional", "default-features"}
        elif dev_features:
            keys.add("features")
            if isinstance(value, dict) and "features" in value and not (
                    isinstance(value["features"], list)
                    and all(isinstance(feature, str) for feature in value["features"])):
                return False
        return (isinstance(value, dict) and isinstance(value.get("path"), str)
                and set(value) <= keys
                and workspace_crate(posixpath.join(posixpath.dirname(path), value["path"])))

    def workspace_dependency(value) -> bool:
        return isinstance(value, dict) and value == {"workspace": True}

    def eligible(table: str, key: str, value) -> bool:
        if table == 'features' and key != 'default' and sign == '+':
            previous = other_document.get('features', {}).get(key)
            for name, features in feature_transfers:
                if (previous is not None and features.get(key) == previous
                        and value == [name + '/' + key, *previous]
                        and local_dependency(dependencies.get(name))):
                    return True
        if donors and new_crate:
            if table == 'features' and key != 'default':
                return (any(doc.get('features', {}).get(key) == value for doc in donors)
                        and isinstance(value, list)
                        and all('/' in item or item in document.get('features', {}) for item in value))
            parsed_table = tomllib.loads('[' + table + ']\n') if table else {}
            def entries(doc):
                for component in table.split('.') if not table.startswith('target.') else ():
                    doc = doc.get(component, {})
                if table.startswith('target.'):
                    target = next(iter(parsed_table.get('target', {})), None)
                    kind = table.rsplit('.', 1)[-1]
                    scope = doc.get('target', {}).get(target, {})
                    # A new crate's test-only use may inherit a former runtime
                    # dependency, but never change its target predicate/value.
                    if kind == 'dev-dependencies':
                        return {**scope.get('dependencies', {}), **scope.get(kind, {})}
                    return scope.get(kind, {})
                return doc
            if table in {'dependencies', 'dev-dependencies', 'build-dependencies'} or table.startswith('target.'):
                return any(entries(doc).get(key) == value for doc in donors)
        if table == "lints":
            return key == "workspace" and value is True and len(document["lints"]) == 1
        if table == "package" and new_crate:
            return key in {"name", "version", "edition", "publish", "license", "description"}
        if table == "workspace" and key == "members":
            if not isinstance(value, list):
                return False
            for member in value:
                if not isinstance(member, str):
                    return False
                if sign == "+":
                    if not read_file(member + "/Cargo.toml"):
                        return False
                elif member not in other_root.get("members", []):
                    if not read_file(member) or read_other(member):
                        return False
            return True
        if table in {"dependencies", "dev-dependencies", "build-dependencies"}:
            return (local_dependency(value, conservative=sign == "+" or key in surviving_dependencies,
                                     dev_features=sign == "+" and table == "dev-dependencies")
                    or (new_crate and (workspace_dependency(value)
                                       or value in (external_values or {}).get((None, table, key), [])
                                       or (table in ('dev-dependencies', 'build-dependencies')
                                           and value in (external_values or {}).get((None, 'dependencies', key), [])))))
        if table == "features" and key != "default" and isinstance(value, list) and (value or new_crate):
            for feature in value:
                if not isinstance(feature, str) or not re.fullmatch(r"[\w-]+\??/[\w-]+", feature):
                    return False
                dependency = feature.split("/")[0].removesuffix("?")
                dep = dependencies.get(dependency)
                if not local_dependency(dep, conservative=False):
                    inherited = root.get("dependencies", {}).get(dependency)
                    if not (workspace_dependency(dep) and isinstance(inherited, dict)
                            and isinstance(inherited.get("path"), str)
                            and workspace_member(posixpath.normpath(inherited["path"]), root)
                            and read_file(posixpath.normpath(inherited["path"]) + "/Cargo.toml")):
                        return False
            return True
        return False

    allowed, forbidden = set(), set()
    table = ""
    header = None
    entries = []
    pending = []
    start = 0
    bin_index = -1
    source_lines = source.splitlines()

    def dotted_dependency():
        if not table or table == '[bin]':
            return None
        parsed = tomllib.loads('[' + table + ']\n')
        prefix = ''
        if 'target' in parsed:
            target, parsed = next(iter(parsed['target'].items()))
            prefix = 'target.' + json.dumps(target) + '.'
        for kind in ('dependencies', 'dev-dependencies', 'build-dependencies'):
            if kind in parsed and len(parsed[kind]) == 1:
                return prefix + kind, next(iter(parsed[kind]))
        return None

    def finish_table(end):
        if table == "[bin]":
            if bin_index in bins:
                allowed.update(range(header, end))
            else:
                forbidden.update(range(header, end))
            return
        if new_crate and dotted_dependency():
            block = tomllib.loads("\n".join(source_lines[header - 1:end - 1]))
            base_table, key = dotted_dependency()
            if 'target' in block:
                block = next(iter(block['target'].values()))
            kind = base_table.rsplit('.', 1)[-1]
            ok = eligible(base_table, key, block[kind][key])
            (allowed if ok else forbidden).update(range(header, end))
            return
        if (header is not None and (table in {"dependencies", "dev-dependencies", "build-dependencies", "features", "lints"}
                                   or (donors and table.startswith('target.'))
                                   or (new_crate and table == "package"))
                and entries and all(entries)):
            allowed.add(header)
        elif header is not None and (new_crate or table == "lints"):
            forbidden.add(header)

    for number, line in enumerate(source_lines, 1):
        stripped = line.strip()
        if not pending and stripped.startswith("["):
            finish_table(number)
            table = stripped.split("#", 1)[0].strip().removeprefix("[").removesuffix("]")
            header, entries = number, []
            if table == "[bin]":
                bin_index += 1
            continue
        if table == "[bin]" or (new_crate and dotted_dependency()):
            continue
        if not pending and (not stripped or stripped.startswith("#")):
            if new_crate:
                allowed.add(number)
            continue
        if not pending:
            start = number
        pending.append(line)
        try:
            entry = tomllib.loads("\n".join(pending))
        except tomllib.TOMLDecodeError:
            continue
        ok = len(entry) == 1 and all(eligible(table, key, value) for key, value in entry.items())
        entries.append(ok)
        if ok:
            allowed.update(range(start, number + 1))
        elif (new_crate or table == "lints"
              or (table == "features" and ("default" in entry or any(value == [] for value in entry.values())))
              or (table == "workspace" and "members" in entry)
              or (table in {"dependencies", "dev-dependencies", "build-dependencies"}
                  and any(isinstance(value, dict) and "path" in value for value in entry.values()))):
            forbidden.update(range(start, number + 1))
        pending = []
    finish_table(len(source_lines) + 1)
    return allowed, forbidden


def crate_move_claims(out: str, read_old, read_new, rewrites,
                      external_values=None, hints=None) -> dict[int, str]:
    """Claim bounded crate wiring before applying the legacy classifications.

    Rewrite pairs are restricted to the same diff file (including a git-detected
    rename), one removed and one added occurrence each. Maps are explicit,
    simultaneous, token-boundary path substitutions on the removed side only.
    Contextual failures cannot fall through to the legacy use/include waivers.
    Imports have a separate whole-file leaf-multiset proof; alias expansion in
    bodies and relative paths in custom #[path] mounts still require review.
    Basenames and module prefixes cannot prove identity between delete/add files
    (notably repeated mod.rs names). Such pairs retain residue and get a hint to
    separate the git mv from its rewrite rather than guessing their pairing.
    """
    claims = {}
    blocks = [[]]
    for raw in out.splitlines():
        if ANSI.sub("", raw).startswith("diff --git "):
            blocks.append([])
        blocks[-1].append(raw)
    bins = transferred_bins(blocks, read_old, read_new)
    renames = rename_pairs(blocks)
    # Byte-identical literals/payloads may move between files. Prove that with
    # the complete diff inventory, not a per-file count that rejects extraction.
    inventories = {'-': set(), '+': set()}
    for lines in blocks:
        plain = [ANSI.sub('', line) for line in lines]
        for sign, prefix, rename_prefix, reader in (
                ('-', '--- a/', 'rename from ', read_old),
                ('+', '+++ b/', 'rename to ', read_new)):
            file = next((line[len(prefix):] for line in plain if line.startswith(prefix)), None)
            file = file or next((line[len(rename_prefix):] for line in plain if line.startswith(rename_prefix)), None)
            if file and file.endswith('.rs'):
                source = reader(file)
                inventories[sign].update(source[a:b] for a, b in text_identity_regions(source))
    donor_paths = {}
    for old, new in renames.items():
        old_root, _, _ = crate_context(old, read_old)
        new_root, _, _ = crate_context(new, read_new)
        if old_root and new_root and old_root != new_root and not read_old(new_root + '/Cargo.toml'):
            donor_paths.setdefault(new_root, set()).add(old_root + '/Cargo.toml')
    lock_allowed = lockfile_lines(read_new('Cargo.lock'), read_old('Cargo.lock'),
                                  read_old, read_new, donor_paths) if donor_paths else set()
    offset = 0
    deleted_paths, added_paths = [], []
    for lines in blocks:
        plain = [ANSI.sub("", line) for line in lines]
        old_path = next((line[4:].removeprefix("a/") for line in plain if line.startswith("--- ")), None)
        new_path = next((line[4:].removeprefix("b/") for line in plain if line.startswith("+++ ")), None)
        old_path = old_path or next((line[12:] for line in plain if line.startswith('rename from ')), None)
        new_path = new_path or next((line[10:] for line in plain if line.startswith('rename to ')), None)
        path = new_path if new_path != "/dev/null" else old_path
        added = old_path == "/dev/null"
        if path and path.endswith(".rs"):
            if added:
                added_paths.append(path)
            elif new_path == "/dev/null":
                deleted_paths.append(path)
        skeleton = False
        if path and added and re.fullmatch(r"crates/[^/]+/src/(?:lib|main)\.rs", path):
            skeleton = skeleton_lines(read_new(path))
        manifest = path and path.endswith("Cargo.toml") and not skeleton
        eligible, forbidden = {}, {}
        new_manifest = bool(added and path and re.fullmatch(r"crates/[^/]+/Cargo.toml", path))
        if manifest:
            donor_files = donor_paths.get(posixpath.dirname(path), ())
            precedents = (external_dependency_values(donor_files, read_old)
                          if donor_files else external_values)
            transfers = []
            for root, donors in donor_paths.items():
                if path in donors:
                    doc = tomllib.loads(read_new(root + '/Cargo.toml'))
                    transfers.append((doc['package']['name'], doc.get('features', {})))
            for sign, file_path, reader in (("-", old_path, read_old), ("+", new_path, read_new)):
                source = reader(file_path) if file_path != "/dev/null" else ""
                other = read_new if sign == "-" else read_old
                eligible[sign], forbidden[sign] = manifest_wiring(
                    source, file_path, reader, other, sign, new_manifest, precedents,
                    new_path if sign == "-" else old_path, bins.get((sign, file_path), set()),
                    tuple(tomllib.loads(read_old(p)) for p in donor_files), transfers
                ) if source else (set(), set())
        numbers = {"-": 0, "+": 0}
        keys = {}
        proved = set()
        import_lines = {'-': set(), '+': set()}
        import_ok = False
        import_remainders = {}
        protected = {'-': set(), '+': set()}
        attribute_protected = {'-': set(), '+': set()}
        macro_protected = {'-': set(), '+': set()}
        if path and path.endswith('.rs') and old_path != '/dev/null' and new_path != '/dev/null':
            before, after = read_old(old_path), read_new(new_path)
            recorded = recorded_macro_names(before,
                rust_path_resolver(old_path, read_old, rewrites, new_path, read_new), rewrites)
            new_recorded = recorded_macro_names(after, rust_path_resolver(new_path, read_new, []), rewrites)
            protected = changed_region_lines(before, after,
                text_identity_regions(before, recorded, build_source_spans(before) if path.endswith('/build.rs') else ()),
                text_identity_regions(after, new_recorded, build_source_spans(after) if path.endswith('/build.rs') else ()),
                inventories)
            macro_protected = changed_region_lines(before, after,
                opaque_macro_offsets(before, recorded), opaque_macro_offsets(after, new_recorded), inventories)
            # Includes and verified build-source consumers have their own byte
            # proof below; their changed string spellings are not data edits.
            for sign, source in (('-', before), ('+', after)):
                for number, row in enumerate(source.splitlines(), 1):
                    if 'include_str!' in row or 'include_bytes!' in row:
                        protected[sign].discard(number)
                        macro_protected[sign].discard(number)
                if path.endswith('/build.rs'):
                    for a, b in build_source_spans(source):
                        macro_protected[sign].difference_update(
                            range(source.count('\n', 0, a) + 1, source.count('\n', 0, b) + 2))
            old_attrs, old_regions = item_attributes(before)
            new_attrs, new_regions = item_attributes(after)
            old_headers = {header for _, header in old_attrs if 'use' not in header}
            new_headers = {header for _, header in new_attrs if 'use' not in header}
            old_tokens = ' '.join(t for _, t, _ in rust_tokens(before))
            new_tokens = ' '.join(t for _, t, _ in rust_tokens(after))
            common_headers = {header for header in old_headers | new_headers
                              if ' '.join(header) in old_tokens and ' '.join(header) in new_tokens}
            def comparable_attributes(records, header):
                inline_to_decl = (len(header) >= 2 and header[-2] == 'mod'
                                  and ' '.join(header) + ' {' in old_tokens
                                  and ' '.join(header) + ' ;' in new_tokens)
                values = []
                for attrs, h in records:
                    if h != header:
                        continue
                    if inline_to_decl:
                        # Existing tests-out wiring can introduce #[path].
                        # An external-module redirect has no old inline body
                        # and must keep the path in its identity.
                        attrs = tuple(re.sub(r'\[ path = "[^"\n]+" \]', '', ' '.join(attrs)).split())
                    values.append(attrs)
                return values
            changed_headers = {header for header in common_headers
                               if comparable_attributes(old_attrs, header)
                               != comparable_attributes(new_attrs, header)}
            for sign, source, records, regions in (
                    ('-', before, old_attrs, old_regions), ('+', after, new_attrs, new_regions)):
                for (_, header), (a, b) in zip(records, regions):
                    owned = set(range(source.count('\n', 0, a) + 1, source.count('\n', 0, b) + 2))
                    # Import attributes are checked with their expanded leaves.
                    protected[sign].difference_update(owned)
                    if header in changed_headers:
                        attribute_protected[sign].update(owned)
            if old_path != new_path:
                pattern = r'\binclude_(?:str|bytes)!\s*\(\s*"([^"\n]+)"\s*\)'
                def includes(source):
                    tokens = list(rust_tokens(source))
                    starts = {pos for i, (pos, token, _) in enumerate(tokens[:-1])
                              if token in ('include_str', 'include_bytes') and tokens[i + 1][1] == '!'}
                    matches = [match for match in re.finditer(pattern, source) if match.start() in starts]
                    # Unsupported syntax is unproved, not an empty include list.
                    return matches if len(matches) == len(starts) else None
                old_includes = includes(before)
                new_includes = includes(after)
                if old_includes is None or new_includes is None or len(old_includes) != len(new_includes) or any(
                        read_old(posixpath.normpath(posixpath.join(posixpath.dirname(old_path), a[1])), raw=True) is None
                        or read_old(posixpath.normpath(posixpath.join(posixpath.dirname(old_path), a[1])), raw=True)
                        != read_new(posixpath.normpath(posixpath.join(posixpath.dirname(new_path), b[1])), raw=True)
                        for a, b in zip(old_includes, new_includes)):
                    claims[offset] = 'blocked'
            if rewrites or (crate_context(old_path, read_old)[0] != crate_context(new_path, read_new)[0]):
                old_leaves, import_lines['-'], old_remainder = import_identity(
                    before, rust_path_resolver(old_path, read_old, rewrites, new_path, read_new, renames))
                new_leaves, import_lines['+'], new_remainder = import_identity(
                    after, rust_path_resolver(new_path, read_new, []))
                import_ok = old_leaves is not None and new_leaves is not None and old_leaves == new_leaves
                if import_ok:
                    before, after = old_remainder, new_remainder
                import_remainders = {'-': old_remainder.splitlines(), '+': new_remainder.splitlines()}
            keys['-'] = contextual_lines(before, old_path, read_old,
                                          rewrites, renames, new_path, read_new, proved)
            keys['+'] = contextual_lines(after, new_path, read_new, [], {}, recorded_macros=new_recorded)
        removed, additions = [], []
        for index, line in enumerate(plain):
            hunk = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
            if hunk:
                numbers = {"-": int(hunk[1]), "+": int(hunk[2])}
            elif line.startswith(("--- ", "+++ ")):
                continue
            elif line.startswith(("+", "-")):
                sign = line[0]
                absolute = offset + index
                import_line = numbers[sign] in import_lines[sign]
                if numbers[sign] in attribute_protected[sign] or numbers[sign] in macro_protected[sign] or (
                        numbers[sign] in protected[sign]):
                    claims[absolute] = 'blocked'
                elif path and not path.endswith(('.rs', 'Cargo.toml', 'Cargo.lock')):
                    claims[absolute] = 'blocked'
                elif import_line and not import_ok:
                    claims[absolute] = 'blocked'
                elif import_line and not import_remainders[sign][numbers[sign] - 1].strip():
                    claims[absolute] = 'rewrites'
                elif skeleton:
                    claims[absolute] = "skeletons"
                elif path == 'Cargo.lock' and sign == '+' and numbers[sign] in lock_allowed:
                    claims[absolute] = 'manifests'
                elif path == 'Cargo.lock' and donor_paths and line[1:].strip():
                    claims[absolute] = 'blocked'
                elif manifest and numbers[sign] in forbidden[sign]:
                    claims[absolute] = "blocked"
                elif manifest and numbers[sign] in eligible[sign]:
                    claims[absolute] = "skeletons" if new_manifest else "manifests"
                elif keys and (rewrites or renames) and (import_line or not MOVED_RE.match(lines[index])):
                    key = keys[sign][numbers[sign] - 1]
                    if import_line or (not COMMENT.match(line) and ('::' in line or 'include_str!' in line or 'include_bytes!' in line)):
                        claims[absolute] = 'blocked'
                    (removed if sign == "-" else additions).append(
                        (absolute, key, line[1:], numbers[sign] in proved or import_line))
                numbers[sign] += 1
            elif line.startswith(" "):
                numbers["-"] += 1
                numbers["+"] += 1
        if keys:
            available = {}
            for index, body, original, _ in additions:
                available.setdefault(body, []).append((index, original))
            for index, body, original, mapped in removed:
                matches = available.get(body, [])
                if matches and (mapped or old_path != new_path) and (body != original or matches[-1][1] != original):
                    other, _ = matches.pop()
                    claims[index] = claims[other] = "rewrites"
        offset += len(lines)
    if rewrites and hints is not None:
        for old_path in deleted_paths:
            for new_path in added_paths:
                if posixpath.basename(old_path) == posixpath.basename(new_path):
                    hints.append(f"  hint: {old_path} -> {new_path} is a delete/add pair; "
                                 "land the git mv and the rewrite as separate commits.")
    return claims


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", nargs="?")
    parser.add_argument("--cached", action="store_true")
    parser.add_argument("--show-all", action="store_true")
    parser.add_argument("--rewrite", type=rewrite_map, action="append", default=[])
    args = parser.parse_args()
    if bool(args.target) == args.cached:
        parser.error("give one commit/range or --cached")
    target = "--cached" if args.cached else args.target
    show_all = args.show_all

    diff_args = [
        "git",
        "-c", "color.diff.oldMoved=magenta",
        "-c", "color.diff.newMoved=cyan",
        "-c", "color.diff.old=red",
        "-c", "color.diff.new=green",
        # Crate moves are git mv plus path edits; rename pairing keeps a moved
        # file's rewritten lines in one diff block whatever the user config.
        "-c", "diff.renames=true",
        "diff",
        "--color=always",
        "--color-moved=plain",
        "--color-moved-ws=no",
    ]
    if target == "--cached":
        diff_args.append("--cached")
    elif ".." in target:
        diff_args.append(target)
    else:
        diff_args.append(f"{target}^!")

    out = subprocess.run(diff_args, capture_output=True, text=True, check=True).stdout

    if target == "--cached":
        old_ref, new_ref = "HEAD", ""
    elif "..." in target:
        base, new_ref = target.split("...", 1)
        new_ref = new_ref or "HEAD"
        old_ref = subprocess.check_output(["git", "merge-base", base or "HEAD", new_ref], text=True).strip()
    elif ".." in target:
        old_ref, new_ref = target.split("..", 1)
        old_ref, new_ref = old_ref or "HEAD", new_ref or "HEAD"
    else:
        old_ref, new_ref = target + "^", target

    cache = {}

    def reader(ref):
        def read(path, raw=False):
            key = (ref, path)
            if key not in cache:
                result = subprocess.run(["git", "show", f"{ref}:{path}"], capture_output=True)
                cache[key] = result.stdout if result.returncode == 0 else None
            # Raw reads preserve line endings and distinguish empty from missing.
            return cache[key] if raw else (cache[key] or b"").decode()
        return read

    # Strip only ANSI for file boundaries; retain moved colors for legacy classes.
    old_reader, new_reader = reader(old_ref), reader(new_ref)
    external_values = {}
    if re.search(r"^\+\+\+ b/crates/[^/]+/Cargo.toml$", ANSI.sub("", out), re.MULTILINE):
        paths = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", old_ref], text=True).splitlines()
        external_values = external_dependency_values(
            [path for path in paths if path.endswith("Cargo.toml")], old_reader)
    hints = []
    claims = crate_move_claims(out, old_reader, new_reader, args.rewrite, external_values, hints)
    counts, residue = classify(out, claims)
    residue, vis_pairs = drop_visibility_pairs(residue)
    residue, include_pairs = drop_include_str_prefix_pairs(residue)
    residue.extend(ANSI.sub("", raw) for index, raw in enumerate(out.splitlines())
                   if claims.get(index) == "blocked")
    scaffold = counts["scaffold"]

    print(
        f"moved lines: {counts['moved']}  allowlisted wiring: {counts['allowed']}  "
        f"comment lines: {counts['comments']}  scaffold: {scaffold}  "
        f"visibility pairs: {vis_pairs}  include_str pairs: {include_pairs}  "
        f"crate skeletons: {counts['skeletons']}  manifest wiring: {counts['manifests']}  "
        f"path rewrites: {counts['rewrites']}  "
        f"residue: {len(residue)}"
    )
    if vis_pairs:
        print(f"  note: {vis_pairs} signature(s) widened visibility (fn -> pub(crate) fn "
              f"etc.) — required wiring when private items move across module walls.")
    if include_pairs:
        print(f"  note: {include_pairs} include_str! path(s) had their leading '../' depth "
              f"rewritten — required wiring when a test mod moves deeper (D6).")
    if scaffold > SCAFFOLD_CAP:
        print(f"  scaffold {scaffold} EXCEEDS cap {SCAFFOLD_CAP}: a dispatch-split commit")
        print("  may not carry this many structural lines — bulk semantics must not hide")
        print("  in scaffold. Split into smaller slices (fewer domain modules per commit).")
        return 1
    if residue:
        limit = len(residue) if show_all else 40
        for line in residue[:limit]:
            print(f"  RESIDUE {line}")
        if len(residue) > limit:
            print(f"  … {len(residue) - limit} more (--show-all to print)")
        for hint in hints:
            print(hint)
        print("NOT a pure move. Residue lines are unmatched changes; split the commit")
        print("or justify each line in review.")
        return 1
    if scaffold:
        print(f"PURE MOVE PROVEN: every non-scaffold changed line is a detected move "
              f"({scaffold} dispatch-split scaffold line(s), within cap {SCAFFOLD_CAP}).")
    else:
        print("PURE MOVE PROVEN: every non-wiring changed line is a detected move.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
