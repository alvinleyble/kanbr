# kanbr

A Kanban board for [Firstmate](https://github.com/kunchenguid/firstmate) work inside [Herdr](https://herdr.dev).

Kanbr shows everything your Firstmate crew is doing on one board: what is booked,
what is ready to start, what is being built, and what just shipped. Decisions that
are waiting on you are flagged in red and pinned to the top, so none of them hide
in a busy column.

Kanbr is **read-only**. It reads Firstmate's state and never changes it.

```text
 Kanbr   1 All 29   2 firstmate 7   3 kanbr 4   4 Leyble-Hub 12   5 Portfolio 1   6 jobibi 4
 ⚑ 6 waiting on you  ·  w or click to jump
 Booked 12         │ Ready 4           │ Building 3        │ Dev 3            │ Staging 0  │ Live 7
───────────────────│───────────────────│───────────────────│──────────────────│────────────│──────────
▌Leyble-Hub        │▌firstmate         │▌kanbr             │▌Leyble-Hub   #141│            │▌firstmate  #9
▌⚑ grill Print que…│▌fleet-sync prunes…│▌Kanbr slice 1: th…│▌Bulk selection f…│            │▌Bring firstm…
▌1d · your call    │▌35d · queued      │▌opus-5-5 · 33m · …│▌1d · merged      │            │▌today · merged
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
| **Dev**, **Staging**, **Live** | Just-finished work (last 7 days by default). A project uses a lane when its repository has the branch that backs it (by default `dev`, `staging`, `main`). Merged work lands in the first lane its project uses, so a card skips lanes its project does not have. Finished work with no PR (such as a report) is shown in the project's last lane. |

In this release Dev, Staging, and Live show only just-finished work. Tracking each
change as it is promoted from branch to branch comes in a later release.

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
branch, worktree, pane, PR link, last status event, and notes.

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

# The branch that backs each release lane. A project that does not have a
# branch never shows cards in that lane. Leave a value empty to turn a lane
# off for every project, for example: staging_branch =
dev_branch = dev
staging_branch = staging
live_branch = main

# Words (comma-separated, case-insensitive) in a hold reason or title that mark
# a Booked item as needing a grill, and words in a hold reason that mark its
# whole project as halted.
grill_words = grill
halted_words = halted

# How many days finished work stays in Dev, Staging, or Live.
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
- each project's repository can be read for the configured Dev, Staging, and Live
  branches;
- the snapshot actually produces cards (open backlog rows with no cards means the
  data shape changed).

```text
  ok    snapshot          bin/fm-fleet-snapshot.sh --json ran in 1.1s, schema fm-fleet-snapshot.v1
  ok    backlog rows      27 structured rows carry every field Kanbr reads
  ok    project lanes     firstmate live, Portfolio dev+live, Leyble-Hub dev+staging+live, ...
  ok    board             29 cards (Booked 12 · Ready 4 · Building 3 · Dev 3 · Staging 0 · Live 7), 6 waiting on you, 7 tabs
```

### What Kanbr reads

| Surface | Why |
| --- | --- |
| `bin/fm-fleet-snapshot.sh --json` | Firstmate's canonical fleet snapshot: backlog, workers, and every registered second mate. This is the complete contract that `fm-bearings-snapshot.sh` summarizes; Kanbr needs fields the summary leaves out, such as each item's project and hold kind. |
| `state/<id>.meta` | a worker's model and effort, which no snapshot carries |
| `data/projects.md` | registered project names |
| each project's git refs (`git for-each-ref`) | which of the configured Dev, Staging, and Live branches a project has |

Kanbr runs the snapshot with `FM_HOME` set to your home. It never writes any
Firstmate file. The snapshot may refresh Firstmate's own cached copies of remote
second-mate summaries, as it does whenever Firstmate itself runs it.

### Refresh

Every `interval` seconds, Kanbr checks the modification times of the backlog, the
second-mate registry, and the task status and meta files. It runs the full snapshot
only when one of them changed, or every `refresh` seconds at most, because a
worker's live state can change without any file changing. Elapsed times tick every
second. If a refresh fails, the last good board stays up, with a notice saying how
old it is.

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
