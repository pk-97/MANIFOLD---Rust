# Simplification census tools

Source of the one-off census tools named in `docs/SIMPLIFICATION_CENSUS.md` section 5 (Stages and the sweep split). They run from `/tmp/simplification_census/tools/`. If that folder is gone, recreate each file below at that path and `chmod +x` the `.sh` and `.py` files.

Order: `run_screens.sh` calls `build_corpus.sh`, `census_refs.py` (with `pub_items.yml`), `node_usage.sh`, `param_moves.awk` and `free_fns.yml`.

## run_screens.sh

```bash
#!/bin/bash
# Stage 0 of the simplification census (docs/SIMPLIFICATION_CENSUS.md section 5).
# Runs every mechanical screen read-only against the main checkout and writes
# raw TSV/text into $OUT/raw. Static only: rg, ast-grep, jq, cargo machete.
# Usage: run_screens.sh [OUT]   (default /tmp/simplification_census)
#   STEPS=rest   everything except the pub-reference scan (about 2 minutes)
#   STEPS=pub    only the pub-reference scan; CRATES="manifold-core manifold-io"
#                limits it to those crates (each run appends to summary.txt)
#   SKIP_CORPUS=1 reuses the corpus an earlier run built.
set -u
STEPS="${STEPS:-all}"
if [ "$STEPS" = pub ]; then SKIP_CORPUS=1; SKIP_PUB_REFS=0; fi
if [ "$STEPS" = rest ]; then SKIP_PUB_REFS=1; fi
REPO="/Users/peterkiemann/MANIFOLD - Rust"
T="$(dirname "$0")"
OUT="${1:-/tmp/simplification_census}"
RAW="$OUT/raw"
CORPUS="$OUT/corpus"
CR="$REPO/crates"
TRANSCRIPTS=/Users/peterkiemann/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust
mkdir -p "$RAW/pub_refs"
git -C "$REPO" rev-parse --short HEAD > "$RAW/main_commit.txt"
step() { echo "[$(date +%H:%M:%S)] $*"; }

[ "$STEPS" = pub ] || : > "$RAW/pub_refs/summary.txt"
step "1 corpus"
[ "${SKIP_CORPUS:-0}" = 1 ] || "$T/build_corpus.sh" "$CORPUS"

step "2 pub item references per crate"
if [ "${SKIP_PUB_REFS:-0}" != 1 ]; then
  members=$(rg -o '"crates/(manifold-[a-z0-9-]+)"' -r '$1' "$REPO/Cargo.toml" | sort -u)
  for n in ${CRATES:-$members}; do
    "$T/census_refs.py" "$CR/$n" --corpus "$CORPUS/projects" > "$RAW/pub_refs/$n.tsv" 2>> "$RAW/pub_refs/summary.txt"
  done
fi
[ "$STEPS" = pub ] && { step "pub scan done"; exit 0; }

step "3 unused dependencies"
cargo machete "$REPO" > "$RAW/machete.txt" 2>&1

step "4 nodes and presets against presets and projects"
"$T/node_usage.sh" "$CORPUS" > "$RAW/node_usage.tsv"

step "5 node params against projects"
for f in "$CORPUS"/projects/*.json; do
  jq -r --arg f "$(basename "$f" .json)" '.. | objects | select(has("typeId") and (.params|type=="object")) | .typeId as $t | .params | to_entries[] | [$f, $t, .key, ((.value.value? // .value) | tostring)] | @tsv' "$f" 2>/dev/null
done > "$RAW/node_param_values.tsv"
jq -r '.nodes[] | .type_id as $t | .params[]? | [$t, .name, (.default|tostring)] | @tsv' "$REPO/docs/node_catalog.json" > "$RAW/node_param_defaults.tsv"
awk -f "$T/param_moves.awk" "$RAW/node_param_defaults.tsv" "$RAW/node_param_values.tsv" | sort -t$'\t' -k4,4rn > "$RAW/node_param_moves.tsv"
awk -F'\t' '{k=$2"\t"$3; if (!((k SUBSEP $4) in s)) {s[k SUBSEP $4]=1; nd[k]++}; if (!((k SUBSEP $1) in p)) {p[k SUBSEP $1]=1; np[k]++}} END {for (k in nd) print k"\t"np[k]"\t"nd[k]}' "$RAW/node_param_values.tsv" | sort > "$RAW/node_param_distinct.tsv"

step "6 dead-code and unused suppressions"
rg -n -B3 '#\[(allow|expect)\([^)]*(dead_code|unused)' "$CR" -g '*.rs' > "$RAW/suppressions.txt"

step "7 cargo features and how often each is named"
for toml in "$CR"/*/Cargo.toml; do
  c=$(basename "$(dirname "$toml")")
  awk '/^\[features\]/{f=1;next} /^\[/{f=0} f && /^[a-z0-9_-]+[ \t]*=/{print $1}' "$toml" | while read -r feat; do
    n=$(rg -c --hidden -g '!.git/' -w -F "$feat" "$CR" "$REPO/scripts" "$REPO/.config" -g '*.toml' -g '*.py' -g '*.sh' -g '*.json' -g '*.rs' 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')
    printf '%s\t%s\t%s\n' "$n" "$c" "$feat"
  done
done | sort -n > "$RAW/features.tsv"

step "8 traits: implementors, dyn uses, bounds"
ast-grep scan --inline-rules 'id: t
language: rust
rule: {kind: trait_item}' "$CR" --json=stream 2>/dev/null \
  | jq -r '(.text|capture("trait\\s+(?<n>[A-Za-z_][A-Za-z0-9_]*)").n) + "\t" + .file' \
  | while IFS=$'\t' read -r t f; do
      impls=$(rg -U -c "impl(<[^>]*>)?\s+(\w+::)*$t(<[^{]*>)?\s+for\s" "$CR" "$REPO/plugins" -g '*.rs' -g '!plugins/vendor/**' 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')
      dyns=$(rg -c "dyn\s+(\w+::)*$t\b" "$CR" -g '*.rs' 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')
      bounds=$(rg -c "(:|\+)\s*(\w+::)*$t\b" "$CR" -g '*.rs' 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')
      printf '%s\t%s\t%s\t%s\t%s\n' "$impls" "$dyns" "$bounds" "$t" "${f#$REPO/}"
    done | sort -n > "$RAW/traits.tsv"

step "9 inventory registries: submissions per registry"
rg -U --no-filename -o '^[ \t]*inventory::submit!\s*[{(]\s*(?:[A-Za-z_]\w*::)*([A-Z]\w*)' -r '$1' "$CR" -g '*.rs' | sed 's/^[ \t]*//' | sort | uniq -c > "$RAW/registry_submits.txt"
rg --no-filename -o 'inventory::collect!\((\w+)\)' -r '$1' "$CR" -g '*.rs' | sort -u | while read -r t; do
  n=$(awk -v t="$t" '$2==t {print $1}' "$RAW/registry_submits.txt"); printf '%s\t%s\n' "${n:-0}" "$t"
done | sort -n > "$RAW/registries.tsv"

step "10 free functions defined in three or more crates"
ast-grep scan -r "$T/free_fns.yml" "$CR" --json=stream 2>/dev/null \
  | jq -r '[(.file|capture("crates/(?<c>[^/]+)/").c), (.file|test("/tests/|tests\\.rs$|_tests\\.rs$|/testkit/")), (.text|capture("fn\\s+(?<n>[A-Za-z_][A-Za-z0-9_]*)").n), .file, (.range.start.line+1)] | @tsv' > "$RAW/free_fns_all.tsv"
awk -F'\t' '$2=="false" {k=$3; if (!((k SUBSEP $1) in seen)) {seen[k SUBSEP $1]=1; n[k]++; c[k]=c[k]" "$1}} END {for (k in n) if (n[k]>=3) print n[k]"\t"k"\t"c[k]}' "$RAW/free_fns_all.tsv" | sort -rn > "$RAW/free_fn_dupes.tsv"

step "11 fallback, legacy and compat paths"
rg -c -i 'fallback|legacy|compat|unwrap_or_default\(\)' "$CR" -g '*.rs' | sort -t: -k2 -rn > "$RAW/fallback_files.txt"
rg -n -o 'fn [a-z0-9_]*(fallback|legacy|compat|old_format|pre_v[0-9]|migrat)[a-z0-9_]*' "$CR" -g '*.rs' > "$RAW/fallback_fns.txt"
rg -o --no-filename -m1 '"projectVersion"\s*:\s*"[^"]*"' "$CORPUS/projects" | sed 's/.*: *//' | sort | uniq -c | sort -k2 -V > "$RAW/project_versions.txt"
rg -o '^\s*\("([^"]+)",' -r '$1' "$CR/manifold-core/src/type_id_migration.rs" | sed 's/^ *//' | while read -r id; do
  printf '%s\t%s\n' "$(rg -l -F "\"$id\"" "$CORPUS/projects" | wc -l | tr -d ' ')" "$id"
done | sort -n > "$RAW/type_id_migrations.tsv"

step "12 switches: const bools and MANIFOLD_ environment knobs"
rg -n '^\s*(pub(\([^)]*\))?\s+)?const\s+[A-Z0-9_]+\s*:\s*bool\s*=' "$CR" -g '*.rs' > "$RAW/const_bools.txt"
rg -o --no-filename 'env::var(_os)?\("[A-Za-z0-9_]+"|"MANIFOLD_[A-Z0-9_]+"' "$CR" -g '*.rs' | rg -o '[A-Z][A-Za-z0-9_]+' | sort | uniq -c | sort -n > "$RAW/env_knobs.txt"

step "13 scripts: executable bit, verbs, real runs in 30 days"
fd -e jsonl . "$TRANSCRIPTS" --changed-within 30d > "$RAW/transcripts_30d.txt"
for f in "$REPO"/scripts/*.py "$REPO"/scripts/*.sh; do
  b=$(basename "$f"); s="${b%.*}"
  case "$b" in test_*) continue ;; esac
  x=no; [ -x "$f" ] && x=yes
  runs=$(tr '\n' '\0' < "$RAW/transcripts_30d.txt" | xargs -0 rg -o --no-filename "\"command\":\"[^\"]{0,300}scripts/$b" 2>/dev/null | wc -l | tr -d ' ')
  refs=$(rg -l --hidden -g '!.git/' -F "$b" "$REPO/scripts" "$REPO/.claude" "$REPO/crates" "$REPO/CLAUDE.md" 2>/dev/null | rg -v "scripts/$b\$" | wc -l | tr -d ' ')
  printf '%s\t%s\t%s\t%s\n' "$runs" "$x" "$refs" "$b"
done | sort -n > "$RAW/scripts.tsv"

step "14 docs: Read calls in 30 days and inbound references"
tr '\n' '\0' < "$RAW/transcripts_30d.txt" | xargs -0 rg -o --no-filename '"file_path":"[^"]*/docs/[A-Za-z0-9_./-]+\.md"' 2>/dev/null | sed 's|.*/docs/||; s|"$||' | sort | uniq -c > "$RAW/doc_reads_30d.txt"
for f in "$REPO"/docs/*.md; do
  b=$(basename "$f")
  reads=$(awk -v b="$b" '$2==b {print $1}' "$RAW/doc_reads_30d.txt")
  refs=$(rg -l --hidden -g '!.git/' -g '!docs/README.md' -F "$b" "$REPO" 2>/dev/null | rg -v "docs/$b\$" | wc -l | tr -d ' ')
  printf '%s\t%s\t%s\n' "${reads:-0}" "$refs" "$b"
done | sort -n > "$RAW/docs.tsv"

step "15 open beads that already name cleanup work"
(cd "$REPO" && bd list --status open 2>/dev/null) | rg -i 'dead|unused|legacy|slop|clean|hygiene|stale|orphan|leftover|retire|remove|delete|simplif|duplicate' > "$RAW/open_cleanup_beads.txt"

step "done; raw files in $RAW"
wc -l "$RAW"/*.tsv "$RAW"/*.txt | sort -n | tail -30
```

## build_corpus.sh

```bash
#!/bin/bash
# Usage corpus for the simplification census: every reachable Peter project as
# raw project.json (via project_tool json), plus the bundled presets and prefs.
# Read-only on every source. Never touches Dropbox online-only (dataless) files.
set -u
REPO="/Users/peterkiemann/MANIFOLD - Rust"
PT="$REPO/target/debug/project_tool"
OUT="${1:-/tmp/simplification_census/corpus}"
mkdir -p "$OUT/projects" "$OUT/presets"
rm -f "$OUT"/projects/*.json
rm -rf "$OUT"/presets/*
: > "$OUT/index.tsv"
: > "$OUT/skipped.tsv"
i=0
{
  fd -e manifold --no-ignore . /Users/peterkiemann/Downloads
  fd -e manifold --no-ignore . "$REPO/tests/fixtures"
  fd -e manifold --no-ignore . /Users/peterkiemann/Library/CloudStorage/Dropbox
} | while IFS= read -r f; do
  flags=$(stat -f '%Sf' "$f")
  case "$flags" in
    *dataless*) printf '%s\t%s\n' "$(stat -f '%z' "$f")" "$f" >> "$OUT/skipped.tsv"; continue ;;
  esac
  i=$((i+1))
  name=$(basename "$f" .manifold | tr -c 'A-Za-z0-9._-' '_')
  dest="$OUT/projects/$(printf '%03d' "$i")_$name.json"
  if "$PT" json "$f" > "$dest" 2>/dev/null; then
    printf '%03d\t%s\t%s\n' "$i" "$(stat -f '%Sm' -t '%Y-%m-%d' "$f")" "$f" >> "$OUT/index.tsv"
  else
    rm -f "$dest"
    printf 'FAIL\t%s\n' "$f" >> "$OUT/skipped.tsv"
  fi
done
cp -R "$REPO/crates/manifold-nodes/assets/effect-presets" \
      "$REPO/crates/manifold-nodes/assets/generator-presets" \
      "$REPO/crates/manifold-nodes/assets/scene-modifier-presets" \
      "$REPO/crates/manifold-nodes/assets/reference-presets" "$OUT/presets/"
cp "/Users/peterkiemann/Library/Application Support/MANIFOLD/prefs.json" "$OUT/prefs.json" 2>/dev/null
echo "projects: $(wc -l < "$OUT/index.tsv")  skipped: $(wc -l < "$OUT/skipped.tsv")  size: $(du -sh "$OUT" | cut -f1)"
```

## census_refs.py

```python
#!/usr/bin/env python3
"""Reference census for every unrestricted `pub` item in one crate.

Usage: census_refs.py <crate-dir> [--corpus DIR] [--all]

Lists `pub` items with ast-grep (pub_items.yml beside this file), skipping test
support (tests/, testkit/, *_tests.rs, test_support.rs, inline
`#[cfg(test)] mod x { }` bodies, items carrying their own `#[cfg(test)]`).
For each name it counts whole-word text hits across the whole repo, hidden
dirs included (.git, gitignored build output and plugins/vendor excluded), and
sorts every hit into a bucket:

  own_src / own_test     this crate, product code / test code
  other_src / other_test other crates
  tooling                scripts/ tools/ plugins/ .claude/ .config/ tv-led-mirror/
  asset                  JSON and fixture files (presets, flows, fixtures)
  other_file             any other non-Rust file (wgsl, swift, toml, ...)
  mention                docs, markdown, beads, txt: never counted as a use

Verdicts: DEAD (no use anywhere), TEST-ONLY (only tests use it), CRATE-ONLY
(only its own crate uses it: over-exposed, not dead), USED.
Text search over-counts (a same-named item elsewhere counts as a use), so every
verdict errs towards keeping code. Names shared with common words (new, title,
legacy) are never judged by this tool; read them with ast-grep instead.
With --corpus, DEAD and TEST-ONLY rows also count corpus files that hold the
name (serde field and variant spellings need their own corpus search).
Prints TSV, candidates first; a summary line goes to stderr.
"""
import json
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

REPO = "/Users/peterkiemann/MANIFOLD - Rust"
RULE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pub_items.yml")
NAME_RE = re.compile(r"\b(?:fn|struct|enum|trait|type|static|const)\s+([A-Za-z_][A-Za-z0-9_]*)")
TEST_PATH = re.compile(r"(/tests/|/benches/|/testkit/|(^|/)tests\.rs$|_tests?\.rs$|/test_[^/]*\.rs$|test_support\.rs$)")
MENTION_PATH = re.compile(r"(^docs/|\.md$|^\.beads/|\.jsonl$|\.txt$)")
ASSET_PATH = re.compile(r"(/assets/|^tests/fixtures/|\.json$)")
TOOLING_PATH = re.compile(r"^(scripts/|tools/|plugins/|\.claude/|\.config/|tv-led-mirror/)")
MOD_LINE = re.compile(r"^(\s*)(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{")

_lines_cache = {}
_region_cache = {}


def file_lines(rel):
    if rel not in _lines_cache:
        try:
            with open(os.path.join(REPO, rel), encoding="utf-8", errors="replace") as fh:
                _lines_cache[rel] = fh.readlines()
        except OSError:
            _lines_cache[rel] = []
    return _lines_cache[rel]


def test_regions(rel):
    """Line ranges of inline `#[cfg(test)] mod x { ... }` bodies.

    An out-of-line `#[cfg(test)] mod tests;` is not a region: its body is a
    separate file the path rules already catch. A body ends at the first later
    line holding only `}` at the module's own indent.
    """
    if rel in _region_cache:
        return _region_cache[rel]
    lines = file_lines(rel)
    regions = []
    for i, l in enumerate(lines):
        if not l.strip().startswith("#[cfg(test)]"):
            continue
        for j in range(i + 1, min(i + 4, len(lines))):
            m = MOD_LINE.match(lines[j])
            if m:
                end = len(lines)
                for k in range(j + 1, len(lines)):
                    if lines[k].rstrip("\n") == m.group(1) + "}":
                        end = k + 1
                        break
                regions.append((j + 1, end))
                break
            if lines[j].strip() and not lines[j].strip().startswith("#"):
                break
    _region_cache[rel] = regions
    return regions


def in_test_region(rel, line):
    return any(a <= line <= b for a, b in test_regions(rel))


def cfg_test_attr(rel, line):
    """True when the item at `line` carries its own `#[cfg(test)]`."""
    lines = file_lines(rel)
    k = line - 2
    while 0 <= k < len(lines) and lines[k].strip().startswith(("#[", "///", "//")):
        if re.match(r"#\[cfg\((any\()?test\b", lines[k].strip()):
            return True
        k -= 1
    return False


def pub_items(crate_dir):
    out = subprocess.run(
        ["ast-grep", "scan", "-r", RULE, crate_dir, "--json=stream"],
        capture_output=True, text=True, errors="replace", check=True,
    ).stdout
    items = []
    for line in out.splitlines():
        m = json.loads(line)
        text = re.sub(r"\b(const|async|unsafe|extern\s+\"[^\"]*\")\s+(?=fn\b)", "", m["text"])
        name = NAME_RE.search(text)
        if not name:
            continue
        kind = text.split(name.group(1))[0].split()[-1]
        rel = os.path.relpath(m["file"], REPO)
        start = m["range"]["start"]["line"] + 1
        span = m["range"]["end"]["line"] - m["range"]["start"]["line"] + 1
        if TEST_PATH.search(rel) or in_test_region(rel, start) or cfg_test_attr(rel, start):
            continue
        items.append((name.group(1), kind, rel, start, span))
    return items


def bucket(rel, line, crate_rel):
    if MENTION_PATH.search(rel):
        return "mention"
    if TOOLING_PATH.search(rel):
        return "tooling"
    if rel.endswith(".rs"):
        own = rel.startswith(crate_rel + "/")
        is_test = bool(TEST_PATH.search(rel)) or in_test_region(rel, line)
        return ("own_" if own else "other_") + ("test" if is_test else "src")
    if ASSET_PATH.search(rel):
        return "asset"
    return "other_file"


def census(item, crate_rel, corpus):
    name, kind, rel, line, span = item
    out = subprocess.run(
        ["rg", "--hidden", "-g", "!.git/", "-g", "!plugins/vendor/", "-n", "-w", "-F",
         "--no-heading", name, REPO],
        capture_output=True, text=True, errors="replace",
    ).stdout
    counts = {}
    for hit in out.splitlines():
        parts = hit.split(":", 2)
        if len(parts) < 3:
            continue
        hrel = os.path.relpath(parts[0], REPO)
        ln = int(parts[1])
        if hrel == rel and ln == line:
            continue
        b = bucket(hrel, ln, crate_rel)
        counts[b] = counts.get(b, 0) + 1
    code = sum(v for k, v in counts.items() if k != "mention")
    tests = counts.get("own_test", 0) + counts.get("other_test", 0)
    if code == 0:
        verdict = "DEAD"
    elif code == tests:
        verdict = "TEST-ONLY"
    elif code == counts.get("own_src", 0) + tests:
        verdict = "CRATE-ONLY"
    else:
        verdict = "USED"
    corpus_hits = ""
    if corpus and verdict in ("DEAD", "TEST-ONLY"):
        c = subprocess.run(["rg", "-l", "-w", "-F", name, corpus], capture_output=True, text=True, errors="replace").stdout
        corpus_hits = str(len(c.splitlines()))
    return (verdict, name, kind, f"{rel}:{line}", span, counts, corpus_hits)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    crate_dir = os.path.abspath(args[0])
    crate_rel = os.path.relpath(crate_dir, REPO)
    corpus = args[args.index("--corpus") + 1] if "--corpus" in args else None
    show_all = "--all" in args
    items = pub_items(crate_dir)
    with ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(lambda it: census(it, crate_rel, corpus), items))
    order = {"DEAD": 0, "TEST-ONLY": 1, "CRATE-ONLY": 2, "USED": 3}
    rows.sort(key=lambda r: (order[r[0]], r[3]))
    cols = ["own_src", "own_test", "other_src", "other_test", "tooling", "asset", "other_file", "mention"]
    print("verdict\tname\tkind\tdef\tlines\t" + "\t".join(cols) + "\tcorpus_files")
    summary = {}
    for verdict, name, kind, loc, span, counts, corpus_hits in rows:
        summary[verdict] = summary.get(verdict, 0) + 1
        if verdict == "USED" and not show_all:
            continue
        print("\t".join([verdict, name, kind, loc, str(span)] + [str(counts.get(c, 0)) for c in cols] + [corpus_hits]))
    print(f"# {crate_rel}  " + "  ".join(f"{k}={v}" for k, v in sorted(summary.items(), key=lambda kv: order[kv[0]])), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

## pub_items.yml

```yaml
id: pub-item
language: rust
rule:
  all:
    - any:
        - kind: function_item
        - kind: struct_item
        - kind: enum_item
        - kind: trait_item
        - kind: type_item
        - kind: const_item
        - kind: static_item
        - kind: function_signature_item
    - has:
        kind: visibility_modifier
        regex: '^pub$'
```

## node_usage.sh

```bash
#!/bin/bash
# Node and preset usage census. For every registered node type id and every
# bundled preset id, count where the exact quoted id string appears:
# bundled presets, real projects (corpus), Rust code outside its own definition,
# tests. Prints TSV sorted with the least-used first.
# Usage: node_usage.sh [corpus-dir]   (default /tmp/simplification_census/corpus)
set -u
REPO="/Users/peterkiemann/MANIFOLD - Rust"
CORPUS="${1:-/tmp/simplification_census/corpus}"
ASSETS="$REPO/crates/manifold-nodes/assets"
count_files() { rg -l -F "$1" "${@:2}" 2>/dev/null | wc -l | tr -d ' '; }
printf 'kind\tid\tpresets\tprojects\trust_src\trust_test\n'
{
  jq -r '.nodes[].type_id | "node\t" + .' "$REPO/docs/node_catalog.json"
  jq -r '.presets[] | .kind + "\t" + .id' "$REPO/docs/node_catalog.json"
  for f in "$ASSETS"/scene-modifier-presets/*.json; do
    printf 'scene_modifier\t%s\n' "$(jq -r '.presetMetadata.id // .id // .name' "$f")"
  done
} | while IFS=$'\t' read -r kind id; do
  q="\"$id\""
  presets=$(count_files "$q" "$ASSETS" -g '*.json')
  projects=$(count_files "$q" "$CORPUS/projects")
  src=$(rg -l -F "$q" "$REPO/crates" -g '*.rs' -g '!**/tests/**' -g '!*tests.rs' -g '!*_test.rs' 2>/dev/null | wc -l | tr -d ' ')
  test=$(rg -l -F "$q" "$REPO/crates" -g '**/tests/**' -g '*tests.rs' -g '*_test.rs' 2>/dev/null | wc -l | tr -d ' ')
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$kind" "$id" "$presets" "$projects" "$src" "$test"
done | sort -t$'\t' -k4,4n -k3,3n
```

## param_moves.awk

```awk
# Joins node_param_defaults.tsv (type, param, default) with
# node_param_values.tsv (project, type, param, value) and prints, per param:
# type, param, default, projects_with_type, projects_storing_param,
# projects_where_moved. A param no project ever moved is a cut candidate.
BEGIN { FS = OFS = "\t" }
NR == FNR { def[$1 SUBSEP $2] = $3; next }
{
  typeproj[$2 SUBSEP $1] = 1
  k = $2 SUBSEP $3
  if (!(k in def)) next
  stored[k SUBSEP $1] = 1
  d = def[k]; v = $4
  if (v == "true") v = 1; if (v == "false") v = 0
  if (d == "true") d = 1; if (d == "false") d = 0
  numeric = (v ~ /^-?[0-9.eE+-]+$/ && d ~ /^-?[0-9.eE+-]+$/)
  if (numeric ? ((v - d) > 1e-6 || (d - v) > 1e-6) : (v != d)) moved[k SUBSEP $1] = 1
}
END {
  for (tp in typeproj) { split(tp, a, SUBSEP); ntype[a[1]]++ }
  for (s in stored) { split(s, a, SUBSEP); nstored[a[1] SUBSEP a[2]]++ }
  for (m in moved) { split(m, a, SUBSEP); nmoved[a[1] SUBSEP a[2]]++ }
  for (k in def) {
    split(k, a, SUBSEP)
    print a[1], a[2], def[k], ntype[a[1]] + 0, nstored[k] + 0, nmoved[k] + 0
  }
}
```

## free_fns.yml

```yaml
id: free-fn
language: rust
rule:
  kind: function_item
  not:
    inside:
      any:
        - kind: impl_item
        - kind: trait_item
      stopBy: end
```
