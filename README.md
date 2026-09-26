# kanbr

A Kanban board for [Firstmate](https://github.com/kunchenguid/firstmate) work inside [Herdr](https://herdr.dev).

Kanbr shows everything your Firstmate crew is doing on one board: what is booked,
what is ready to start, what is being built, and how far each finished change has
travelled from Dev through Staging to Live. Decisions that are waiting on you are
flagged in red and pinned to the top, so none of them hide in a busy column.

Kanbr is **read-only**. It reads Firstmate's state and your projects' git history,
and never changes either.

```text
 Kanbr   1 All 36   2 firstmate 7   3 kanbr 4   4 Leyble-Hub 21   5 Portfolio 2
 ⚑ 6 waiting on you  ·  w or click to jump
 Booked 5           │ Ready 1           │ Building 1        │ Dev 0             │ Staging 12             │ Live 1
                    │                   │                   │                   │ 12 to promote · 2 db c…│ 10 Sep · #122 · v1.2.1…
────────────────────│───────────────────│───────────────────│───────────────────│────────────────────────│────────────────────────
▌Leyble-Hub         │▌Leyble-Hub        │▌Leyble-Hub        │                   │▌Leyble-Hub         #144│▌Leyble-Hub         #122
▌⚑ grill Print que… │▌Populate the Set… │▌Facebook Lite-st… │                   │▌Remove the refresh bu… │▌fix(server): staging-…
▌1d · your call     │▌today · queued    │▌opus-5-5 · 1h · … │                   │▌today · merged         │▌16d · merged
```

## The board

**Tabs.** `All` comes first, then one tab per project that has work on the board,
with its card count. Tabs appear and disappear on their own. Halted projects keep
their tab (greyed) while they still have items. Switch tabs with the number keys,
`Tab`, or a click.

**Columns.** Every tab always shows all six columns in the same order, so `All`
lines up:

| Column | What lands there |
| --- | --- |
| **Booked** | Held backlog items: a decision or grill waiting on you, or any other hold. A `grill` tag marks items that need a grill. Items of a halted project are greyed and paused. |
| **Ready** | Queued work that is not held. An item waiting on another task stays here with a `⧗` dependency badge. |
| **Building** | Work in flight: every live Firstmate worker. |
| **Dev**, **Staging**, **Live** | Merged changes, placed by git in the furthest branch each has reached (see [Releases](#releases)). **Live** shows only the latest production release. |

### Releases

Dev, Staging, and Live come from each project's git history, so a promotion made
anywhere, including directly on GitHub, moves its cards on the next refresh.

- **Which lanes a project uses.** A project uses a lane when its repository has the
  branch that backs it (by default `dev`, `staging`, `main`). All six columns always
  show; a card skips a lane its project does not use, so a project with no staging
  branch goes from Dev straight to Live, and one with only `main` goes from Building
  to Live. A project missing a configured branch never shows cards in that lane.
- **Where a card sits.** Each merged card keeps its PR. Kanbr finds the commit the PR
  landed as (from GitHub's merge message, `Merge pull request #N` or `Title (#N)`)
  and checks which lane branches contain it. The card sits in the furthest lane
  reached. Merge-commit and fast-forward promotions are found by ancestry; squash
  promotions by matching the promoted content to a point on the lower branch (even
  when the branches have diverged, for example after a hotfix on `main`); rebased or
  cherry-picked promotions by patch id. Changes still on their way stay in Dev or
  Staging however long ago they merged.
- **Live is the latest release.** A release is the newest landing on the Live branch:
  a promotion, a merged PR, or a direct commit (a rebase merge that lands several
  commits at once counts as one). Live shows only the changes that release brought,
  and clears when the next one lands. A fast-forward promotion (such as
  `git push origin staging:main`) leaves no commit of its own, so the release starts
  at the Live branch's commit before it: the base of the merged promotion PR, asked
  of `gh`, else the previous entry in the Live ref's reflog (with several releases
  between two fetches, the reflog sees them as one). With neither, the Live header
  says `release boundary unknown` and no change is claimed for the release.
- **Changes made outside Firstmate.** A merged PR or direct commit with no Firstmate
  card (a hand-opened PR, a manual commit) still shows, as a plain grey card titled
  from the PR or commit, so release headers and notes stay complete. Promotion PRs
  are not cards: they are what moves the cards, and the Live header links the one
  that made the release.
- **Finished work with no PR** (a report, a local task) is shown in the project's
  last lane until that project's next release, for at most `finished_days`.
- **As of the last fetch.** Kanbr never fetches. It reads each clone's
  remote-tracking branches (`origin/dev` and so on, falling back to local branches),
  so the board shows what the forge had when the clone last fetched. A merged card
  whose PR is not in the clone yet falls back to the first lane, and a notice says so.
  The release details show when each clone last fetched.

**Release headers.** Under the Staging and Live titles, a line sums up the release
for the project tab (or the whole board on All):

- **Live**: the release date, the PR that made it, the app version where the
  project has one (an Android `versionName`, or the version in the root `Cargo.toml`,
  `pyproject.toml`, or a published `package.json`), and the change count.
- **Staging**: the next release: how many changes are ready to promote, and how many
  database migrations Staging has that Live does not (files under a `migrations` or
  `migrate` directory, such as `supabase/migrations` or `server/db/migrations`).

Press `i`, or click a Dev, Staging, or Live header, for the full release details:
the promotion PR link, each database migration, and the branches read.

**Release notes.** Press `R` for short plain-language notes for the Live release,
ready to send to users: the changes grouped under New, Improved, and Fixed, with
conventional-commit prefixes such as `feat(orders):` dropped, and internal changes
(docs, tests, CI, chores, and changes known only by a merge message) summed up in
one line. Press `c` to copy them, or select them with the mouse. `kanbr notes` prints the same text.

**Waiting on you.** A decision waiting on you, whether a held backlog item or a
worker (yours or a second mate's) that stopped to ask, gets a red `⚑` badge. The
card stays in its real column. The red strip at the top counts them; press `w`, or click the
strip, to jump from one to the next.

**Cards.** Each card has three lines:

1. the project and PR number (plus a `2nd` tag for a second mate's work);
2. the title, after its badges: `⚑` decision, a test badge (reserved for Firstmate's
   test results, which are not published yet), `grill`, and `⧗` dependency;
3. the worker's model, elapsed time, and state.

Press `Enter` (or double-click a card) for the details: hold reason, blockers,
branch, worktree, pane, PR link, the commit a change landed as, last status event,
and notes.

A plain grey card is a merged change with no Firstmate card (see
[Releases](#releases)).

**Second mates.** Work from every registered Firstmate second mate is mixed into the
same project tabs and columns, marked `2nd`.

**Nothing silently missing.** When the board cannot show something (a second mate's
home is unreadable, a free-form backlog row, a refresh that failed), a yellow notice
line says so. Press `n` to list them all.

### Keys

| Key | Action |
| --- | --- |
| `←` `→` / `h` `l` | move between columns |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn`, `g` `G` | move between cards |
| `Enter`, double-click | card details |
| `1`-`9`, `0`, `Tab`, click | switch project tab (`1` is All) |
| `w`, click the strip | jump to the next card waiting on you |
| `n` | list notices |
| `i`, click a Dev, Staging, or Live header | release details |
| `R` | release notes (`c` copies them) |
| `r` | refresh now |
| `?` | help |
| `q` | quit |

## Install

### As a Herdr plugin

```sh
herdr plugin install alvinleyble/kanbr
```

Herdr 0.9.1 or newer is required. On install, the plugin's build step
(`scripts/install.sh`) puts the `kanbr` binary at `target/release/kanbr` inside the
plugin checkout:

1. it downloads the prebuilt binary for this version and platform (macOS and Linux,
   arm64 and x86_64) from the matching GitHub release and checks its SHA-256, so no
   Rust toolchain is needed;
2. if no prebuilt binary matches (no release for this version yet, an unsupported
   platform, or a failed download), it builds from source with `cargo build
   --release --locked`. That needs Rust from [rustup.rs](https://rustup.rs).

The plugin adds one action, **Open Kanbr board** (`kanbr.open`). It opens the board
in its own `Kanbr` workspace. If the board is already open there, the action
focuses it; if you quit the board, the action starts it again in the same pane. To
bind it to a key, add this to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+k"
type = "plugin_action"
command = "kanbr.open"
description = "open Kanbr"
```

### The `kanbr` terminal command

The same binary is a plain terminal command, whatever AI tool you run. Either link
the one the plugin installed:

```sh
ln -sf "$(ls -d ~/.config/herdr/plugins/github/kanbr-*/target/release/kanbr | head -n 1)" ~/.local/bin/kanbr
```

or install it with Cargo:

```sh
cargo install --git https://github.com/alvinleyble/kanbr --locked
```

### Point Kanbr at your Firstmate home

Kanbr looks for the Firstmate home in this order, and stops at the first one set:

1. `--home PATH`
2. the `KANBR_FM_HOME` environment variable
3. the `FM_HOME` environment variable
4. `home = PATH` in the config file
5. the current directory, or one of its parents
6. `~/firstmate`

If you set a home explicitly (steps 1 to 4) and it is not a Firstmate home, Kanbr
stops with an error instead of trying the next place. The Herdr action does not see
your shell's environment, so for the plugin, set `home` in the config file.

## Usage

```sh
kanbr                        # open the board
kanbr print                  # print the board once as text
kanbr print --tab Leyble-Hub # one project only
kanbr notes --tab Leyble-Hub # release notes for the latest Live release
kanbr doctor                 # check every Firstmate surface Kanbr reads
kanbr open                   # open the board in its own Herdr workspace (the plugin action)
```

## Configuration

Kanbr is one public tool that you adapt with a small config file. You don't need to
fork it. The file is optional. Kanbr reads the first of these that exists:

1. the file given with `--config PATH`
2. `$HERDR_PLUGIN_CONFIG_DIR/config` (when Herdr runs the action; see `herdr plugin config-dir kanbr`)
3. `$XDG_CONFIG_HOME/kanbr/config`, or `~/.config/kanbr/config`

Each line is `key = value`, and lines starting with `#` are comments. Every key is
optional; the defaults match the workflow Kanbr was designed for:

```ini
# Firstmate home (see "Point Kanbr at your Firstmate home").
# home = ~/firstmate

# Column labels.
booked_label = Booked
ready_label = Ready
building_label = Building
dev_label = Dev
staging_label = Staging
live_label = Live

# The branch that backs each release lane (each must be different). A
# project that does not have a branch never shows cards in that lane. Leave a
# value empty to turn a lane off for every project, for example:
# staging_branch =
dev_branch = dev
staging_branch = staging
live_branch = main

# Words (comma-separated, case-insensitive) in a hold reason or title that mark
# a Booked item as needing a grill, and words in a hold reason that mark its
# whole project as halted.
grill_words = grill
halted_words = halted

# How many days finished work that git does not place stays on the board:
# finished work with no PR (also cleared by the project's next release), and
# merged work whose PR is not in the project's clone yet.
finished_days = 7

# Seconds between cheap change checks, and the most seconds between full reads.
interval = 2
refresh = 15
```

An unknown key or a bad value is an error, so a typo can't be silently ignored.

## `kanbr doctor`

`kanbr doctor` checks every Firstmate surface Kanbr reads and exits non-zero if any
of them is broken. Run it after every Firstmate update, so a change that breaks
Kanbr shows up right away rather than as a silently empty board.

It checks:

- the config file parses, and the Firstmate home resolves;
- `bin/fm-fleet-snapshot.sh` exists, is executable, runs, and reports schema
  `fm-fleet-snapshot.v1`;
- every backlog row, worker row, and second-mate record carries each field Kanbr
  maps onto the board. It names any field that is missing;
- the second-mate registry is readable, and every registered home is readable;
- each live worker's `state/<id>.meta` is readable and names a model;
- `data/projects.md` lists the registered projects;
- each project's clone can be read: the branch behind each of Dev, Staging, and Live,
  the latest release, database changes waiting in Staging, and when the clone last
  fetched (a clone of a project on the board that has not fetched for a week is a
  warning, since the board lags the forge until it does);
- `gh` is installed and signed in (optional: it is only asked about a merged PR whose
  merge message has no PR number, and about the promotion PR behind a fast-forward
  release). A fast-forward release whose start is unknown is a warning;
- the snapshot actually produces cards (open backlog rows with no cards means the
  data shape changed).

```text
  ok    snapshot          bin/fm-fleet-snapshot.sh --json ran in 2.5s, schema fm-fleet-snapshot.v1
  ok    backlog rows      26 structured rows carry every field Kanbr reads
  ok    project git       firstmate: origin/main; latest release 25 Sep #9; fetched 18h ago
  ok    project git       Leyble-Hub: origin/dev origin/staging origin/main; latest release 10 Sep #122 v1.2.1; 2 database change(s) waiting in staging; fetched 38m ago
  ok    gh                installed and signed in; asked only about a merged PR whose merge message has no PR number, and the promotion PR behind a fast-forward release
  ok    board             36 cards (Booked 12 · Ready 2 · Building 2 · Dev 0 · Staging 12 · Live 8), 6 waiting on you, 7 tabs
```

### What Kanbr reads

| Surface | Why |
| --- | --- |
| `bin/fm-fleet-snapshot.sh --json` | Firstmate's canonical fleet snapshot: backlog, workers, and every registered second mate. This is the complete contract that `fm-bearings-snapshot.sh` summarizes; Kanbr needs fields the summary leaves out, such as each item's project and hold kind. |
| `state/<id>.meta` | a worker's model and effort, which no snapshot carries |
| `data/projects.md` | registered project names |
| each project's git history | which configured branches back Dev, Staging, and Live; how far each merged change has reached; the latest release, app version, and database migrations. Read-only: `for-each-ref`, `log`, `rev-list`, `merge-base`, `diff-tree`, `cherry`, `patch-id`, `ls-tree`, `cat-file`, the Live ref's reflog (`log -g`, only after a fast-forward release), and the modification time of `FETCH_HEAD` |
| `gh pr view` (optional) | the commit a merged PR landed as, only for a Firstmate card whose PR number is in no merge message |
| `gh pr list` (optional) | the merged promotion PR behind a fast-forward to the Live branch, and the branch's commit before it: where that release starts |

Kanbr runs the snapshot with `FM_HOME` set to your home. It never writes any
Firstmate file or project repository, and never fetches. The snapshot may refresh
Firstmate's own cached copies of remote second-mate summaries, as it does whenever
Firstmate itself runs it. Kanbr reads the git history only of projects on the board
(a halted project's is not read), so dormant repositories never add tabs.

### Refresh

Every `interval` seconds, Kanbr checks the modification times of the backlog, the
second-mate registry, and the task status and meta files. It runs the full snapshot
only when one of them changed, or every `refresh` seconds at most, because a
worker's live state can change without any file changing. Elapsed times tick every
second. If a refresh fails, the last good board stays up, with a notice saying how
old it is.

Git work happens only in these full reads, never while drawing. Each read checks
each project's branch tips (one `git for-each-ref`); a project's history is
analysed again only when a lane branch moved or the clone fetched, and results
that cannot change for the same commits are kept for the whole session. The
first read after start takes a moment longer while every project is analysed.

## Development

```sh
cargo test                                   # unit and end-to-end tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs the format check, build, clippy, tests, and the install script on Linux
and macOS for every pull request.

To publish prebuilt binaries, bump `version` in both `Cargo.toml` and
`herdr-plugin.toml` (a test keeps them equal), merge, then push the tag
`v<version>`. The release workflow builds macOS and Linux binaries for arm64 and
x86_64, and attaches them with SHA-256 checksums to a GitHub release. Plugin
installs download those binaries.
