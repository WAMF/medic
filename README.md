# medic

`medic` is a triage nurse for pull requests. It is a small Rust CLI that reads
only a local git repository. It answers two questions during a merge triage:

1. **`medic overlap`**: Which files does each change touch? Which changes share
   files? Which changes really conflict? Which set of changes can I merge
   together without a conflict?
2. **`medic risk`**: How risky is it to merge this one change? It gives a
   level (low, medium, high), a score from 0 to 100, and the reasons.

## Rules medic keeps

- **Git only.** medic runs the `git` binary. It does not use `gh`, a GitHub
  API, or a git library.
- **No network**, except `git fetch`, and only when you pass `--fetch`.
- **Three-dot diffs.** A change is `git diff base...head`, the same diff a
  GitHub pull request shows. Commits that land on the base after the branch
  was cut are not counted as part of the change.
- **The work tree is never touched.** Conflicts come from
  `git merge-tree --write-tree`, which merges in memory. medic does not check
  out, does not change the index, and does not move any branch. It writes only
  unreferenced objects, which `git gc` removes.
- **Text for people, `--json` for agents.**

Requirements: git 2.38 or later (for `merge-tree --write-tree`). A clone with
enough history to find the merge base (not `--depth 1`). Building needs Rust
1.88 or later (CI tests that minimum).

## Install

Inside a Digital Workforce workspace, use the platform tool:

```sh
.dw/tools/medic/bin/run --help
```

From source:

```sh
cargo install --path .
```

Static Linux builds (no C toolchain needed, the Rust linker is enough):

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
cargo build --release --locked --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl
```

## Usage

```text
medic [-C DIR] [--json] [--config FILE] <COMMAND>

Commands:
  overlap  List the files each change touches, flag overlaps and real conflicts,
           and suggest a set of changes that merge together cleanly
  risk     Score the merge risk of one change (base...head) as low, medium or high
  config   Print the risk config in effect (or the built-in defaults) as TOML
```

### Naming changes

Every change argument is a git ref: a branch, a full ref, a commit, or a pull
request number.

- `123` or `#123` is a pull request number. An argument made only of digits
  is always a PR number, never a branch or a commit id.
- With `--fetch <remote>`, medic first runs one `git fetch`:
  - a PR number fetches `refs/pull/<n>/head` into `refs/medic/pull/<n>`;
  - a branch name fetches `refs/heads/<name>` into
    `refs/remotes/<remote>/<name>` (or `refs/medic/heads/<name>` when the
    remote is a URL). `<remote>/<name>`, for example `origin/main` with
    `--fetch origin`, names the same branch;
  - a full `refs/...` name (for example `refs/tags/v1.2`) fetches into
    `refs/medic/...`;
  - `HEAD` and commit ids (7 to 40 hex digits) are not fetched; they must
    already be local.
- Without `--fetch`, a PR number uses `refs/medic/pull/<n>` from an earlier
  fetch. Anything else is passed to git as it is.

You can also fetch yourself and pass plain refs:

```sh
git fetch origin main 'refs/pull/*/head:refs/remotes/origin/pr/*'
medic overlap --base origin/main origin/pr/101 origin/pr/102
```

### `medic overlap`

```sh
medic overlap --fetch origin --base main 101 102 103 104
```

```text
base main (57cfc5b944)

Changes (4):
  #101     5 files     357 lines  risk low     24.3  merges cleanly
  #102     3 files      57 lines  risk low       10  merges cleanly
  #103     3 files      15 lines  risk low       20  merges cleanly
  #104   254 files   14579 lines  risk high     100  CONFLICTS WITH BASE
      base conflict: a.yml, b.yml, c.ts, d.ts, e.dart, and 9 more

Shared files (1):
  dw/skills/code-review/SKILL.md  <- #102, #103

Conflicts between changes (0):
  none

Overlap without conflict (1):
  #102 + #103: dw/skills/code-review/SKILL.md

Suggested merge set (3 of 4):
  1. #102
  2. #103
  3. #101
Left out:
  #104: conflicts with the base; rebase it first
```

What it does, step by step:

1. **Files.** For each change it runs
   `git diff --name-status -M base...head` and `git diff --numstat -M`.
   Renames are detected (`-M`). A rename counts as a change to both the old
   and the new path, so a rename and an edit of the old file show as an
   overlap.
2. **Base check.** `git merge-tree --write-tree base head`. A change that does
   not merge cleanly into the base is marked `CONFLICTS WITH BASE`.
3. **Pair check.** For each pair (A, B) it first merges A onto the base in
   memory, wraps the result in a dangling commit (`git commit-tree`), and then
   runs `git merge-tree --write-tree <base+A> B`. This answers the real
   question: *after A is merged, does B still merge?* A pair with a shared
   file but a clean merge is an **overlap**. A pair whose merge fails is a
   **conflict**. By default every pair is checked. `--overlapping-only` checks
   only pairs that share a file, which is faster for big groups but can miss
   rare conflicts that have no shared path (for example a directory rename).
4. **Risk.** Each change also gets a `medic risk` score, with its conflicts
   and overlaps folded in.
5. **Suggested set.** See the rule below.

`--files` lists every changed file in the text output.

Documentation means `*.md`, `*.mdx`, `*.rst`, `*.adoc`, licence and changelog
files, and text, HTML and image files under `docs/` or `doc/`. A build or code
file such as `CMakeLists.txt` or `docs/conf.py` is not documentation. A rename
counts as documentation only when both its old and new paths are
documentation, so moving code into `docs/` still counts as code. `--avoid-overlap` also
keeps changes that share a file out of the suggested set.

#### The suggested-set rule

Finding the *largest* set of changes with no pairwise conflict is the maximum
independent set problem, which is NP-hard. medic uses a greedy rule and then
verifies it:

1. Drop changes that conflict with the base.
2. Order the rest by number of pairwise conflicts (fewest first), then by risk
   score (lowest first), then by input order. Changes that block few others
   go first, so more changes fit.
3. Walk that order. Skip a change if it conflicts with one already taken
   (with `--avoid-overlap`, also if it shares a file).
4. Otherwise merge it in memory on top of everything taken so far. Keep it
   only if that merge is clean. This catches conflicts that only appear when
   three or more changes combine.

The result is **maximal**: no change left out can be added without a
conflict. It is not always **maximum**. The order in `merge_order` is the
order medic verified, so merging in that order is known to be clean at the
time of the run.

#### JSON shape

```jsonc
{
  "base": { "label": "main", "ref": "refs/remotes/origin/main", "commit": "…" },
  "prs": [{
    "label": "#101", "ref": "refs/medic/pull/101", "commit": "…", "merge_base": "…",
    "lines": 357,
    "files": [{ "path": "src/new.rs", "old_path": "src/old.rs", "status": "R", "added": 3, "deleted": 1 }],
    "base_conflict": false,
    "risk": { "score": 24.3, "level": "low", "reasons": ["…"], "factors": [/* … */], "metrics": { /* … */ } }
  }],
  "pairs": [{ "a": "#102", "b": "#103", "shared_files": ["…"], "conflict": false }],
  "shared_files": [{ "path": "…", "prs": ["#102", "#103"] }],
  "suggestion": { "rule": "…", "merge_order": ["#102", "#103", "#101"],
                  "excluded": [{ "label": "#104", "reason": "conflicts with the base; rebase it first" }] }
}
```

`pairs[].conflict` is `null` when a pair was not checked (one side conflicts
with the base, or `--overlapping-only` skipped it). `binary` files have
`added` and `deleted` set to `null`.

### `medic risk`

```sh
medic risk --base main feature/payments
medic risk --fetch origin --base main 101 --against 102 103
medic --json risk --base origin/main HEAD --fail-on high
```

```text
#6013 (b69756ef9c) vs main (57cfc5b944)
risk: MEDIUM  score 32.8/100
11 files (6 code, 2 test, 3 docs, 0 binary), 218 lines (90 code, 115 test), 3 code directories

Reasons:
  +15 migrations: touches database migrations or schema: backend/convex/convex/schema.ts
  +10 hotspot: backend/convex/convex/schema.ts changed in 166 base commits in the last 90 days
  +5 bugfix_history: backend/convex/convex/schema.ts had 42 fix commits in the window
  +1.3 spread: code changes span 3 directories
  +1.1 size: 90 changed lines in code files (218 in total)
  +0.4 files: 6 code files changed (11 files in total)
```

- `--against REF...` also checks the change against other changes, one pair
  per `--against` ref (the `--against` refs are not checked against each
  other). A conflict
  with one of them adds points; a shared file without a conflict adds a few.
- `--fail-on LEVEL` exits with code 3 when the level is at or above `LEVEL`.
  Use it as a CI gate.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Runtime failure: not a repository, unknown ref, bad config, git failed. |
| 2 | Usage error (bad flags). |
| 3 | `medic risk --fail-on` threshold met. |

## The risk model

### Research

Merge risk is the chance that a change brings a defect, breaks the build, or
is hard to merge. Research on *just-in-time* (per-change) defect prediction
and on code review points to a small set of signals. Each signal below can be
computed from git alone.

| Signal | Evidence | In medic |
| --- | --- | --- |
| **Size** (lines changed) | Size is one of the strongest single predictors of a defect-inducing change [1][2]. Review effectiveness falls above about 200–400 changed lines; past that, reviewers find fewer defects per line [7]. | `size` |
| **Number of files** | The number of files a change touches predicts defects [1][2]. | `files` |
| **Diffusion / spread** across directories and modules | Mockus and Weiss found the *diffusion* of a change across files and modules is essential to predicting its risk [1]. Kamei et al. confirm subsystems, directories and entropy as predictors [2]. Hassan shows that scattered changes predict faults [5]. | `spread` |
| **Churn and hotspots** (how often the touched files change) | Relative code churn predicts defect density well (89% accuracy on Windows Server 2003) [3]. Files that changed recently and often are likely to hold the next fault [4][6]. | `hotspot` |
| **Bug-fix history** of the touched files | Files with recent bug fixes are likely to have more bugs. This is the "bug cache" idea [4], and Google's deployed "Rahman" ranking of files by fix commits [6]. | `bugfix_history` |
| **Developer experience** | Low experience with the system raises the risk of a change [1][2]. Low ownership relates to more defects [8]. | `experience` |
| **Tests changed with code** | Practice-based, not a predictor from the papers above. A code change with no test change gives reviewers no executable evidence, so its defects are more likely to escape. The weight is modest and configurable. | `tests` |
| **Merge conflicts** | Code involved in a merge conflict is about twice as likely to have a bug. When the conflict needs manual resolution, it is 26× as likely [9]. | `base_conflict`, `pr_conflict`, `pr_overlap` |
| **Sensitive paths**: CI, auth and security, migrations, dependencies, lockfiles, infrastructure, binaries | Not a defect predictor in the papers above. These paths have a larger *blast radius*: a bad CI change blocks everyone; a bad migration or auth change is hard to undo; dependency and lockfile changes are a supply-chain surface [10]; binary files cannot be reviewed as text. This is a practice-based weight, so it is configurable. | `ci`, `security`, `migrations`, `dependencies`, `lockfiles`, `infra`, `binary` |
| **Documentation-only changes** | Docs do not run. A docs-only change with no conflict and no sensitive path is low risk by construction. | `docs_only` cap |

What medic does **not** use: the change's text description (needs the hosting
API), review comments, and CI results. These also predict risk [2], but they
are not in git. Combine medic's score with them in the agent that calls it.

A caution from Google's deployment [6]: a score that is opaque, slow or
noisy is ignored. So medic prints every factor, its points and the plain
reason, and every weight lives in a config file.

### Scoring

Each factor adds points. The score is the sum, capped at 100.

- **Ramp factors** (`size`, `files`, `spread`, `hotspot`, `bugfix_history`)
  give 0 points at or below `low`, the full `weight` at or above `high`, and
  a straight line between.
- **`experience`** is an inverted ramp: an author with `low` or fewer
  earlier commits on the base gets the full weight; one with `high` or more
  gets 0.
- **`tests`**: when at least `min_code_lines` code lines changed, the points
  are `weight × (1 − min(1, ratio / target_ratio))`, where `ratio` is test
  lines over code lines.
- **Sensitive categories** add their flat points once when any changed path
  matches.
- **Conflicts**: `conflict.base` if the change does not merge into the base,
  `conflict.other` if it conflicts with another change, `conflict.overlap` if
  it only shares files.
- **Docs-only cap**: if every file is documentation, no path is sensitive or
  binary, and there is no conflict, the score is capped at
  `docs_only.max_score`.

"Code" means any file that is not docs, tests, generated output, or a
lockfile. `size`, `files` and `spread` count code only, so a regenerated
lockfile or a large docs page does not inflate them.

Levels: `score ≥ levels.high` is **high**, `score ≥ levels.medium` is
**medium**, else **low**.

Defaults:

| Factor | Default |
| --- | --- |
| levels | medium 30, high 60 |
| size (code lines) | weight 25, low 50, high 1000 |
| files (code files) | weight 15, low 5, high 40 |
| spread (code directories) | weight 10, low 2, high 10 |
| tests | weight 10, min_code_lines 20, target_ratio 0.5 |
| hotspot (commits on busiest file) | weight 10, low 3, high 20, last 90 days, max 5000 commits read |
| bugfix_history (fix commits on a file) | weight 5, low 0, high 5; subject words fix, bug, bugfix, hotfix, revert, regression, broken, with or without the suffix s, es, ed, d or ing (so `fixes` counts and `fixture` does not) |
| experience (author commits) | weight 5, low 5, high 50 |
| conflict | base 40, other 20, overlap 5 |
| sensitive | security 15, migrations 15, ci 10, dependencies 8, infra 8, binary 5, lockfiles 4 |
| docs_only | max_score 10 |

With these defaults:

- A docs-only change with no conflict is **low** (at most 10).
- A change that conflicts with the base is at least **medium** (40).
- About 1000 code lines across 40 files in 10 directories scores 50
  (25 + 15 + 10), which is medium. With no test changes it adds 10 and is
  **high** (60). Hotspots and sensitive paths push it further.

### Config file

medic reads `--config FILE`, or else `.medic.toml` at the repository root,
or else the defaults. A config file only needs the keys it changes; every
other key keeps its default. A list (a path pattern list) replaces the
default list. Unknown keys are an error, so a typo does not pass silently.

```toml
# .medic.toml
[levels]
medium = 25
high = 50

[size]
weight = 30

[conflict]
base = 60

[sensitive]
lockfiles = 0

[paths]
# Replaces the default security patterns.
security = ["**/auth/**", "**/permissions/**", "backend/convex/convex/authz*.ts"]
```

Print the full default config, with every path pattern, to start your own:

```sh
medic config --defaults > .medic.toml
medic config            # shows the config in effect and where it came from
```

Path patterns are globs, matched case-insensitively against the
repository-relative path. `*` stays inside one directory; `**` crosses
directories.

### Sources

1. A. Mockus and D. M. Weiss, "Predicting risk of software changes," *Bell
   Labs Technical Journal* 5(2):169–180, 2000.
   [doi:10.1002/bltj.2229](https://doi.org/10.1002/bltj.2229)
2. Y. Kamei, E. Shihab, B. Adams, A. E. Hassan, A. Mockus, A. Sinha and
   N. Ubayashi, "A large-scale empirical study of just-in-time quality
   assurance," *IEEE TSE* 39(6):757–773, 2013.
   [PDF](https://sail.cs.queensu.ca/data/pdfs/TSE_ALarge-ScaleEmpiricalStudyOfJust-in-TimeQualityAssurance.pdf)
3. N. Nagappan and T. Ball, "Use of relative code churn measures to predict
   system defect density," *ICSE 2005*.
   [doi:10.1145/1062455.1062514](https://doi.org/10.1145/1062455.1062514)
4. S. Kim, T. Zimmermann, E. J. Whitehead Jr. and A. Zeller, "Predicting
   faults from cached history," *ICSE 2007*.
   [doi:10.1109/ICSE.2007.66](https://doi.org/10.1109/ICSE.2007.66)
5. A. E. Hassan, "Predicting faults using the complexity of code changes,"
   *ICSE 2009*. [doi:10.1109/ICSE.2009.5070510](https://doi.org/10.1109/ICSE.2009.5070510)
6. C. Lewis, Z. Lin, C. Sadowski, X. Zhu, R. Ou and E. J. Whitehead Jr.,
   "Does bug prediction support human developers? Findings from a Google case
   study," *ICSE 2013*.
   [Google Research](https://research.google/pubs/pub41145/)
7. SmartBear / Cisco code review study (2006), summarised in
   [Atlassian: code review best practices](https://www.atlassian.com/blog/add-ons/code-review-best-practices)
   and [tekin.co.uk: thousand-line pull requests](https://tekin.co.uk/2020/05/proof-your-thousand-line-pull-requests-create-more-bugs).
8. C. Bird, N. Nagappan, B. Murphy, H. Gall and P. Devanbu, "Don't touch my
   code! Examining the effects of ownership on software quality," *ESEC/FSE
   2011*. [doi:10.1145/2025113.2025119](https://doi.org/10.1145/2025113.2025119)
9. C. Brindescu, I. Ahmed, C. Jensen and A. Sarma, "An empirical
   investigation into merge conflicts and their effect on software quality,"
   *Empirical Software Engineering* 25, 2020.
   [doi:10.1007/s10664-019-09735-4](https://doi.org/10.1007/s10664-019-09735-4)
10. OpenSSF, [Concise Guide for Evaluating Open Source Software](https://best.openssf.org/Concise-Guide-for-Evaluating-Open-Source-Software)
    — dependency changes as a supply-chain risk.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Unit tests cover parsing, config layering, classification and the scoring
model. Integration tests in `tests/cli.rs` build throwaway git repositories
in temp directories and run the real binary: three-dot diffs, renames,
pairwise conflicts, conflicts with the base, the suggested set, `--fetch`
of `refs/pull/<n>/head` from a local "remote", config files and exit codes.

## Versioned releases

The reviewed workflow is `ci/ci.yml`. A human with workflow permission must
copy it to `.github/workflows/ci.yml` before merging this change. GitHub
Actions then runs it on pull requests and main.
After review and merge, create a tag that matches the package version, such
as `v0.1.0`. The tag run tests the code and builds static Linux binaries.
It publishes `medic-linux-amd64`, `medic-linux-arm64`, and `SHA256SUMS`.
The release job requires `gh`. The hosted Ubuntu runner provides it.
A tag that does not match `Cargo.toml` fails before publication.

Digital Workforce downloads these public assets and verifies `SHA256SUMS`.
It does not need a source clone or a private repository token.

To reproduce the release build with Rust 1.88.0:

```sh
rustup target add --toolchain 1.88.0 x86_64-unknown-linux-musl aarch64-unknown-linux-musl
RUSTUP_TOOLCHAIN=1.88.0 bash ci/build-release.sh
```
