# Git worktrees for parallel Claude sessions

A working note on using git worktrees to run multiple concurrent Claude Code
sessions on this repo without them stepping on each other. Also covers cargo
cache behaviour, `jj`, and CLI diff tooling.

## Why worktrees here

A worktree is an additional working directory checked out from the *same* `.git`
object store. Each has its own working files, `HEAD`, and index; they share one
object database and one set of refs. A given branch can only be checked out in
one worktree at a time (git enforces this — that's the isolation you want).

```
git worktree add ../snitchos-featureX -b featureX   # create branch + dir
git worktree list                                    # see them all
git worktree remove ../snitchos-featureX             # clean up
```

For **concurrent sessions** this resolves the tension with the repo's
"everything lands on `main`, no feature branches" rule: that rule is safe for one
session but dangerous for concurrent ones (two agents committing to the same
`main` in the same dir clobber each other's index). Worktrees give each session
its own branch/dir; you merge to `main` at the end — same outcome, no collision.

It also gives **build isolation** (see cargo section) and physical separation
that pairs well with the `snip` staging tool: which change belongs to which
commit is trivial when they're already in separate directories.

**Claude Code shortcut:** the Agent tool's `isolation: "worktree"` spawns an
agent in a temporary worktree that auto-cleans if unchanged — good for fanning
out non-interactive agents. For long-lived *interactive* sessions you drive
yourself, `git worktree add ../snitchos-<topic> -b <topic>` gives a stable
directory to attach a terminal to.

### Caveats
- Worktrees keep work *separate*; they don't *integrate* it. You still merge
  each branch to `main` yourself and resolve conflicts.
- Shared refs cut both ways: a `git gc` / force-delete in one affects the store.
- Logical conflicts still happen — two sessions editing the same `plans/` or
  design doc in separate worktrees will conflict at merge time.

## Cargo cache across worktrees

Two layers, very different behaviour:

- **`~/.cargo/` (downloads, registry index, extracted sources)** — lives in
  home, shared by *all* worktrees and clones automatically. Never the problem.
- **`target/` (compiled artifacts)** — gitignored/untracked, so **per-worktree
  by default**. A second worktree means a full cold compile the first time, then
  its own independent incremental cache.

That independence is what you *want* for concurrent sessions — it's exactly what
avoids the `CARGO_MANIFEST_DIR` env-leak cache-thrash where two build contexts
fought over one `target/`. Cost is disk (one `target/` per worktree) + the cold
first build.

**Don't share `target/` for parallel work.** Cargo locks the target dir during a
build, so two simultaneous `cargo xtask` runs serialize; divergent branches also
invalidate each other's incremental cache. If cold builds hurt, use **`sccache`**
(`RUSTC_WRAPPER=sccache`) — concurrency-safe per-crate compilation cache, no
shared `target/` needed. Sharing via `CARGO_TARGET_DIR` is fine only for
*sequential* work.

## Inspecting a particular worktree

A worktree is just a directory, so point commands at it:

```
ls ../snitchos-featureX                       # plain dir
git -C ../snitchos-featureX status            # git as if cd'd there
git -C ../snitchos-featureX diff              # its unstaged changes
git -C ../snitchos-featureX diff --staged     # its staged changes
```

Because refs are shared, you can inspect another worktree's *committed* state
from anywhere without cd-ing:

```
git diff main..featureX                       # whatever's checked out there
git diff main..featureX -- kernel/            # scoped
```

You only need to *be in* a worktree to see its uncommitted working-dir changes.
`git worktree list` reminds you which branch lives where.

## Does this work with jj?

Yes, and jj arguably fits the parallel-sessions use case *better* — worth a look,
not yet adopted.

- `jj workspace add ../snitchos-x` is the native worktree-equivalent.
- jj continuously auto-commits the working copy into a real commit, so the
  "two agents clobber the same index" hazard is structurally softer — there's no
  staging index to collide on, and jj is built around many concurrent anonymous
  heads rather than one privileged `main`. The "everything on main" rule maps
  onto jj as "small change-stacks you squash down," which is more its grain.
- Diffs are first-class: `jj diff`, `jj diff -r <change>`, `jj log` with inline
  diffs; per-workspace targeting via `jj -R <dir>`.
- **Evaluate colocated** (`.git` + `.jj` side by side) so you keep doing git from
  the CLI while trying jj on top. But don't drive the same repo with both tools
  in the same session — pick one owner of the working-copy state per session.
- jj doesn't change cargo caching — it's the same `target/` on disk.

Switch only if the parallel-session friction is recurring; it's a real
tool-fluency investment.

## Editors + diff tools

- **VS Code** works: treat each worktree as its own folder/window and its SCM
  panel, diffs, and gutters resolve against that worktree's HEAD. Sharp edge:
  one window shows one worktree's git state — open multiple windows, don't manage
  several worktrees from one panel. **GitLens** adds a worktree switcher.
- For a "view diffs, occasional edits, git from CLI" habit, the highest-leverage
  add isn't a new editor — it's a better CLI diff:

**`delta`** — a syntax-highlighting *pager*. Same line-based diff git already
computes, rendered with syntax colors, side-by-side, line numbers, intra-line
highlighting. Safe global default; routes `git diff`/`show`/`log -p`/`blame`.
```ini
[core]
    pager = delta
[interactive]
    diffFilter = delta --color-only
[delta]
    navigate = true
    side-by-side = true
    line-numbers = true
[merge]
    conflictStyle = zdiff3
```

**`difftastic`** — a structural (AST-aware) diff. Parses both sides and diffs the
syntax trees, so it ignores reindent/reflow/brace-move noise — unusually nice for
Rust. Binary is `difft`. It's a diff *engine*, not a pager, and not line-oriented
(doesn't feed `git add -p`), so run it **on-demand** rather than as the global
pager:
```ini
[alias]
    dft = "!GIT_EXTERNAL_DIFF=difft git diff"
```

Recommended combo: `delta` as the everyday pager, `git dft` when a Rust change is
buried under a reindent.

|                          | delta            | difftastic          |
|--------------------------|------------------|---------------------|
| What it does             | pretties lines   | diffs the syntax tree |
| Drop-in for all git cmds | yes              | no (on-demand)      |
| Best at                  | readability      | ignoring reflow/move noise |
| Feeds `add -p` / tooling | yes              | no                  |
