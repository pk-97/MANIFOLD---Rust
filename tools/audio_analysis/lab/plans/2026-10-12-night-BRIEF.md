# Overnight Detector Lab run, 2026-10-12 — orchestrator brief

**Amendment (Peter, 2026-10-12 late): the two release steps are removed.** The plan is now 6 steps: step 1 run
kick_v2_final, step 2 run snare_v2_mid, steps 3 to 6 the four compares (kick mix, snare mix, snare hardneg, snare
coincident). Releases happen in the morning from whichever configs win. Wherever this brief says "8 steps", read 6;
wherever it numbers the compares 5 to 8, read 3 to 6; ignore every row, timing and rule about release steps, and
the wake condition "both releases failed" no longer exists. The final summary line is `all 6 steps passed`.
Everything else stands.

You run one thing tonight: the committed plan `tools/audio_analysis/lab/plans/2026-10-12-night.json` (8 steps) through
`tools/audio_analysis/lab/overnight.py`, on main at or after `8133b104a`. You watch it, you restart or continue it
when a step dies, you write the morning record. You do not improve anything. Peter's two rules: the worst thing we
can do is train on incorrect data, so you never touch labels, configs or lab code; and the night is never killed on
a timing guess, only restarted or continued.

Paths. `R="/Users/peterkiemann/MANIFOLD - Rust"`, `LAB="$R/tools/audio_analysis/lab"`,
`Q=~/.cache/manifold/detector-lab/queue`, scratchpad `S` = your own session scratchpad directory (this file lives in the repo; never write next to it). The repo has a Bash hook: no `cd`,
no inline python (`python3 -c`), no heredoc-fed interpreters; a scratch script is a file in `S` with a shebang and
the exec bit, run by path. Never write a file under `$R/tools/audio_analysis` (an untracked file there makes
`release.py` refuse: "uncommitted changes").

## 0. What is already verified (do not redo)

`lab/preflight.py` on the plan passed all 10 checks at 23:30: tree clean, MPS visible, 110 GB free, all six configs
plan (kick_v2_final -> kick-59cb80340895, snare_v2_mid -> snare-c634ed410b5d, kick_v2_mix -> kick-ac1f3c714e8f,
snare_v2_mix -> snare-5fea1a1c3d7c, snare_v2_hardneg -> snare-4ece8218f5b6, snare_v2_coincident ->
snare-a0169752f0ad), kick verdict store complete, both `--exam-of` runs present, every cache warm (variants included),
GPU lock free, plan loads, 36 GB RAM with 1.96 GB of 3 GB swap already used by the day's agents. None of these six
run ids has a seed saved yet: everything trains tonight. `scripts/dev.py lab-queue` and `lab-preflight` are BROKEN
(overnight.py and preflight.py have no exec bit; dev.py execs them: PermissionError). Start the queue as in section 2.

## 1. Before you start (five minutes)

1. `git -C "$R" status --porcelain -- tools/audio_analysis` prints nothing. `git -C "$R" log --oneline -1` is
   `8133b104a` or later with no later commit touching `tools/audio_analysis` (if there is one, the run ids above
   moved; re-run the preflight through `$S/probe/run_preflight.py` and use its ids).
2. `"$R/scripts/gpu_queue.py" status` says `GPU free` (held reads `GPU busy: <label> (pid ...) since ...`). If held,
   wait; never clear a holder.
3. `ps -axo pid,%cpu,rss,command | rg 'cargo|rustc|run.py|compare.py|release.py' | rg -v rg` shows no cargo build
   and no lab training. If the lead has lanes building, ask the lead to stop them: the queue's paging watchdog kills a
   lab step when the whole machine pages for 60 s, whoever causes it.
4. `vm_stat | rg Pageouts; sysctl vm.swapusage`. Note both numbers in the record (section 5).
5. Lead decision, not yours: launchd runs `scripts/trunk_health.py` at 04:30 (workspace build, nextest, feature
   matrix, gpu_proofs --all, two rt_noise renders; GPU legs at nightly priority). If the plan is still running then, the
   lab's next step waits behind those GPU legs (1-2 h) and the cargo build may trip the paging watchdog. The lead can
   keep it off tonight with `"$R/scripts/gpu_queue.py" reserve --owner detector-lab --reason 'overnight lab 2026-10-12'
   --seconds 36000` (trunk_health defers its whole sweep while a reservation is live; it expires on its own). You
   do not run that command unless the lead told you to.

## 2. Start

Primary (survives your session, outside the Bash sandbox and hook, visible to Peter): open a terminal tab with the
terminal tool and run

    python3 "/Users/peterkiemann/MANIFOLD - Rust/tools/audio_analysis/lab/overnight.py" "/Users/peterkiemann/MANIFOLD - Rust/tools/audio_analysis/lab/plans/2026-10-12-night.json" 2>&1 | tee "$S/night.out"

Fallback if there is no terminal tool: Bash with the sandbox disabled, `run_in_background`, and
`nohup setsid python3 ... > "$S/night.out" 2>&1 &` (same arguments). If the Bash hook refuses the interpreter prefix,
stop and tell the lead; do not chmod or edit anything in the repo.

Within a minute `ls -t $Q | head -1` is `night-2026-10-12_<stamp>`; call it `D`. `$D/summary.txt` is rewritten after
every step. `$D/NN_<kind>_<config>.log` is each step's output:
01_run_kick_v2_final, 02_run_snare_v2_mid, 03_release_kick_v2_final, 04_release_snare_v2_mid,
05_compare_kick_v2_final_vs_kick_v2_mix, 06_compare_snare_v2_mid_vs_snare_v2_mix,
07_compare_snare_v2_mid_vs_snare_v2_hardneg, 08_compare_snare_v2_mid_vs_snare_v2_coincident.

Expected wall time with nothing in the way: step 1 about 25 min (3 seeds, ~7 min each plus a 4 min first build),
step 2 about 25 min, step 3 about 5 min, step 4 about 10 min, step 5 about 40 min (34 variant copies), steps 6-8
about 25-30 min each. About 3.5-4.5 h in all. These are estimates for the record, never a reason to act.

## 3. Watching

Every 15-20 minutes (Monitor tool with an until-loop, or a backgrounded sleep; never a foreground sleep), read:

    tail -n 4 "$D/summary.txt"; tail -n 6 "$D/$(ls -t $D | rg '\.log$' | head -1)"; "$R/scripts/gpu_queue.py" status; vm_stat | rg Pageouts; sysctl vm.swapusage; pgrep -fl 'overnight.py|run.py --child|release.py|compare.py'

How to read a step log. A run step prints `<config>: run <id>, seeds [0, 1, 2], N to train`, then per seed the
child's lines `net without groups [k]: S s` (10 trainings: 4 outer, 6 pairs), `trees without groups ...`, stage lines,
then `<config> seed k: M min; L left, finish about HH:MM`. A release step prints `<config>: cutoff X cuts (a, b) |
{...}`, `<family>-<date>-<sha>: final models in N s`, `release <train_id> -> <dir>`. A compare step prints the A/B
text; its headline lines (`B - A over songs`, `A: F1`, `B: F1`, `songs better / worse`, `  A: coincident`, and the
same prefixed `old exam | `) are copied into summary.txt under the step row, and the whole text lands in
`~/.cache/manifold/detector-lab/compare/<A>_vs_<B>.txt`. `[gpu-queue] waiting for the GPU: held by ...` lines are
normal waiting, not a hang (trunk_health's GPU legs after 04:30, or a landing's proofs).

Summary rows: `step N kind what: exit C[ (KILLED: sustained paging ...)], peak X GB resident, pageouts +P, swap used
M MB, start to end, S s, log NAME`. Final line: `all 8 steps passed`, or `all 8 steps ran; killed for sustained
paging: step ...`, or `STOPPED: step N exited C; not run: ...`. The queue process exits 0, 137 (a watchdog kill
happened, queue finished) or the failing step's code.

Slow is not stuck. The only wedge test: no new log line for 2 hours AND every process of the step's tree at 0% CPU
for 30 minutes of samples AND the GPU lock held by the lab itself (not waiting). Then record `ps` and
`sample <pid> 5` output into `$S/wedge_<step>.txt`, kill that step's process group once (`pkill -9 -g <pgid>` where
the pgid is the gpu_queue.py wrapper's pid), and continue by section 4. Anything short of that: wait.

## 4. When a step fails: rules

The queue stops at the first non-zero exit except a watchdog kill (exit 137, logged `[overnight.py] killed: pageouts
rose at every sample for 60 s`), after which it continues to the next step. Steps 3 and 4 need step 1's seed 0 and
step 2's seeds 0-2; steps 5-8 train for themselves whatever is missing (`compare.py` trains the absent seeds of both
sides), so a killed run step is repaired by the later compare, at the cost of time; the release of that family is not.

Continuing = a continuation plan: copy the not-yet-passed steps VERBATIM (same JSON objects, same order) into
`$S/continue_<n>.json` with `"name": "night-2026-10-12-cont<n>"`, and start it exactly as in section 2 (new `D`;
record both). A run step rerun skips seeds already saved; a compare skips saved runs; a release retrains its final
models (minutes) and overwrites the same train id, which is fine. Never edit a step's content, never add a step,
never pass `--cpu`, `--release` or `--swap-watch-seconds`.

Before any rerun of a killed or wedge-killed run step: in `~/.cache/manifold/detector-lab/runs/<run id>/`, an
`s<seed>.npz` with no `s<seed>.json` beside it is a partial save (the save is not atomic; the json is written after).
Move it to `$S/partial_<run id>_s<seed>.npz` (do not delete). Nothing else under `~/.cache` is ever moved or deleted.

| Failure | How it shows | Response |
|---|---|---|
| Device guard | `refusing to train a net on the CPU (about 3x slower): ...` in a run or compare log, exit 1 within a minute of the step start | Not a rerun case: MPS vanished for the process. Check `torch` sees MPS from the same shell (`$S/probe/run_preflight.py` check 2). If it passes, start a continuation plan once; if it fails again, leave for morning, wake Peter (section 6). |
| Watchdog kill | summary row `exit 137 (KILLED: sustained paging ...)`, log ends with the `[overnight.py] killed` line; queue continues | Find the cause: `ps -axo pid,rss,%cpu,command \| sort -k2 -n \| tail` and whether cargo/rustc ran (trunk_health, a lane). When the foreign load is gone, run a continuation plan for the killed step and everything after it that depends on it (a killed step 1 or 2: steps 3/4 will have failed "lacks seeds" and stopped the queue; continue from the killed step). Second kill of the same step with no foreign load: leave for morning, wake Peter. |
| Run id moved | `run id X is not the planned Y: code or labels changed mid-run`, or a compare retraining a config that was already trained under another id | Someone changed `tools/audio_analysis` or the labels on main. Do not continue; record `git log -3 -- tools/audio_analysis` and the two ids; wake Peter only if the releases are affected, else leave for morning. |
| Release refuses the tree | `tools/audio_analysis has uncommitted changes: a release train_id must name the code that made it` | `git status --porcelain -- tools/audio_analysis` names the file. If it is a stray untracked file you did not create, record it and leave for morning (you do not clean the tree). |
| Release lacks seeds | `run <id> lacks seeds ['s0.npz']: lab/run.py <config> --seeds ...` | The run step before it did not save (killed or failed). Continuation plan from that run step. |
| Release other | `exam songs without an outer prediction`, `predates the stageraw save`, `the app does not run ...` | Leave for morning; these mean code, not weather. |
| Compare refuses a pair | `labels_sha256 differs ...: not a fair A/B`, `exam differs`, `recall_only differs`, `<id> has no seeds [...]`, `--exam-of <id>: no such run` | Labels or code changed between A's and B's training, or a save is missing. One continuation attempt only if a run save is missing; otherwise leave for morning. |
| Verdict store | `no kick verdict store for <id>` or `<song>: not in the kick verdict store` in a snare step | The shipped kick asset changed (someone ran `release.py export --ship`) or the store was removed. Stop; wake Peter: every snare number tonight depends on that store. Never rebuild it yourself. |
| Cache miss mid-run | `<song>: no cached <rule> candidates; run the experiment first` (scoring) or long `<song>: N s` build lines where preflight said warm | A cache file vanished or a source file changed (`source audio changed since the song list was built`). Record it; a build miss just costs time; the `source audio changed` exit is leave-for-morning (never run `registry.py build`). |
| GPU lock | `[gpu-queue] waiting for the GPU: held by <label> (pid)` for a long time | Wait. If the holder pid is dead (`kill -0`) and the lock stays held for 30 min, record it and wake Peter; do not clear it. |
| Disk | `No space left on device` | Should not happen (110 GB free, the night writes under 10 GB). Record `df -h ~`; leave for morning. |
| Swap climbs without a kill | swap used rising past 3 GB in the record but no kill | Just record it; the watchdog is the judge. |
| MPS OOM / Metal wedge | `MPS backend out of memory` in the log (exit 1) or the section 3 wedge test | OOM: continuation plan once for that step; second time leave for morning. Wedge: section 3. |
| Queue process gone, summary not final | `pgrep` empty, summary without `all 8 steps` or `STOPPED` | Your session or terminal died with it. Continuation plan from the step whose row is missing; the partial-save rule applies. |

Twice is the limit: a step that failed twice (any mix of the above) is left for morning and Peter is woken.

## 5. The morning record

Keep `$S/MORNING.md` current after every step (so a dead session leaves the record). For each step: wall seconds,
exit code, killed or not, peak GB resident, pageouts, swap at end (all from the summary row). For each run step, per
seed from `~/.cache/manifold/detector-lab/runs/<run id>/s<seed>.json`: `build_s`, `train_s`, `device`, and the
`net` seconds of the 10 trainings (a 10x spread between trainings is the paging signature from BUG-9ngk8.1.3; say so).
For each release: the train id (`release <id> ->` line), the `cutoff ... cuts ... | {...}` line, and
`~/.cache/manifold/detector-lab/releases/<train id>/release.json` `score`. For each compare: every headline line and
every `old exam | ` line verbatim, plus the `hits change per song ... worst first` line. Also: start and end time of
the queue, the `D` directories, continuation plans and why, machine state at start and end (vm_stat Pageouts, swap),
and anything in section 7 you saw confirmed or contradicted.

## 6. Waking Peter

Only for: a step failed twice; both releases (steps 3 and 4) failed; the queue finished (all steps ran, or STOPPED).
Nothing else, not for slowness, not for trunk_health waits, not for one watchdog kill. How: write
`$S/WAKE_PETER.md` (one paragraph: what, where, what is left), send the same text as your turn's final message
starting `WAKE PETER:` so the app surfaces it, and `osascript -e 'display notification "<one line>" with title
"MANIFOLD night"'` (a local banner; there is no push script in the repo). Then stop acting until spoken to.

## 7. Never

- Never edit labels, configs, experiments, `variant_proofs.json`, `songs.json`, the plan, or any lab code. Never run
  `lab/audit.py`, `registry.py build`, `kick_labels.py`, `snare_labels.py`, `kick_verdict.py build`, `offsets.py`,
  `release.py export`, or anything with `--ship`, `--release`, `--cpu`.
- Never kill a running step for being slow (section 3's wedge test is the one exception, with evidence recorded).
- Never start anything outside the plan or a verbatim continuation of it: no extra configs, seeds, scores, compares,
  cargo builds, GPU work, landings, merges, pulls, checkouts or branch switches on main. Do not land anything into
  `tools/audio_analysis` tonight and tell any agent that asks to wait for morning (a landing there changes every run id).
- Never clear the GPU lock, a reservation, or `gpu.holder`. Never delete anything under `~/.cache` (partial saves are
  moved to `S`).
- Never change `KICK_GOAL_NN_DEVICE`, `PYTORCH_*`, `SNARE_*`, `KICK_GOAL_*` in the environment.

## 8. Reading tomorrow's numbers (from the adversarial review, so the morning read is right)

- The exams grew: kick 38 project songs (24 old + 14 new), snare 43 (29 old + 14 new); the release holdout keeps 11
  kick and 12 snare songs out of every dev exam (affect mendez motion paces skyline are held out in both). The
  `old exam |` lines score 24 of the shipped kick run's 30 songs and 30 of the shipped snare run's 37 (the rest are now
  held out), so they are comparable between A and B tonight, not one-to-one with the shipped 0.892 / 0.700.
- The shipped kick (never trained on the 18 new snare-family songs) scores them at recall 0.945, precision 0.987,
  0.43 false/min against the old held-out songs' 0.839 / 0.928 / 3.99: the new kick labels are prominent MIDI kicks
  with everything doubtful unscored, so the full kick exam reads easier than the old one. motion is a genuinely hard
  song (recall 0.275, labels placed right); concentrate has 0 kick labels (all three kick tracks are muted in the
  saved set but audible in the bounce: the shipped kick is confident at 90-95% of those 620 muted notes) so it only
  ever contributes false fires.
- Kick label timing: on affect break control dream echo paces running skyline touch waypoints the shipped kick's
  first confident candidate sits 15-30 ms before the label (old songs: within +-5 ms). Inside the 35 ms training
  window, so tonight is valid, but these labels are late; expect slightly later decisions on the new songs.
- The shipped snare, through its CPU reference, recall on the new songs: desire 1.0, sol 1.0, skyline 0.971, affect
  0.971, dream 0.943 (precision 0.215: fires on vocal-chop loops), running 0.889, motion 0.818, paces 0.679, pulse
  0.658, break 0.562, go_with_you 0.510, touch 0.408, control 0.212 (18 false/min), fortuity 0.091 (11 labels),
  waypoints 0.022 (10 fires in the whole song), concentrate 0.000 (164 false fires in 3.5 min); echo and mendez have
  no snare labels (negatives only).
  concentrate's and control's snare labels are claps that sit on kick notes (53 of 53 and 128 of 137 within 35 ms of
  a kick note): the kick2 veto of BUG-9ngk8.15 in the flesh, exactly what snare_v2_coincident tests; read the
  `coincident` lines first. waypoints' snare is a sound the shipped snare does not fire on at all; Peter should listen
  before trusting its 46 labels. Expect the full snare exam to read well below the old exam for every config.
- Mix variants in steps 5 and 6: 34 proven copies; three Pattern APRS masters have the kick and snare patterns 10-11
  ms apart and hung_up T3 and both late_night ALEX remasters were proven on the kick pattern alone at 8 ms. If
  `B - A` on a mix compare is negative, those six are the first suspects, not the method.
