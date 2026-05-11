# BelloSaize

BelloSaize is a lightweight native Linux terminal deck for running multiple Codex, Claude, Mistral, and shell sessions in one clickable window.

It is built for the workflow where the heavy work already happens inside the agent terminals, so the harness itself stays simple:

- real embedded terminals via GTK4 + VTE
- automatic tiling for 1, 2, 3, 4, and larger pane counts
- VS Code-inspired workspace shell with a collapsible repo explorer
- click-to-focus panes, double-click-to-zoom headers
- one repo-scoped `Get Up To Date` action with batch targeting
- selected-repo `Commit+Push` flow that also handles push-only repos

This is a normal desktop app. It does not run a browser UI, it does not need `localhost`, and it does not depend on Electron.

## Why

Running several AI coding agents at once gets messy fast:

- terminals drift across workspaces
- it becomes hard to tell which pane belongs to which repo
- switching between sessions takes more mental overhead than it should

BelloSaize keeps those sessions inside one native window, shows the project for each pane, and gives you one place to launch and manage them.

## Interface

The current layout is built around three pieces:

- a left repository explorer sidebar that can be resized with the splitter and collapsed from the header
- per-repository Git state in the explorer, including up-to-date, dirty, ahead, and behind summaries
- click-to-toggle repo selection with click-again-to-clear behavior
- a clear top action row with `Shell`, `Codex`, `Claude`, `Mistral`, and `Custom`
- a repo action row with scope targeting for the selected repo or all repos
- a centered terminal stage that tiles panes more like a lightweight tiling window manager than a traditional IDE tab strip

The styling is intentionally flat and lightweight: darker editor-style surfaces, thin borders, and minimal visual effects.

## Current Behavior

- `1` pane: full window
- `2` panes: split in two
- `3` panes: two on top, one wide pane below
- `4` panes: four-corner layout
- `5-6` panes: three-column grid
- `7+` panes: four-column grid

The focused pane gets a highlighted border. Double-clicking its header toggles zoom mode.

## Dependencies

Ubuntu / Debian:

```bash
sudo apt-get install -y libgtk-4-dev libvte-2.91-gtk4-dev
```

Rust toolchain:

```bash
curl https://sh.rustup.rs -sSf | sh
```

Agent binaries are optional but expected if you want their launcher buttons enabled:

- `codex`
- `claude`
- `mistral`

## Run

From the repo root:

```bash
cargo run --release
```

Or build once and launch the binary directly:

```bash
cargo build --release
./target/release/bellosaize
```

## Use

1. Select a repository from the left explorer.
2. Launch a pane from the top action row with `Shell`, `Codex`, `Claude`, `Mistral`, or `Custom`.
3. Click the selected repository again if you want to clear the current repo target.
4. Double-click a repository row if you want the fastest path to a shell in that folder.
5. Choose `Selected` or `All` in the repo action scope picker before running repo-wide git actions.
6. Use `Get Up To Date` when you want BelloSaize to fetch and then fast-forward only the repos that are safe to update.
7. Click any pane to focus it.
8. Use `Reset` on the focused pane if you want to kill it and relaunch the same repo and command from scratch.
9. Double-click a pane header to zoom or unzoom it.
10. Use the sidebar toggle if you want more room for terminals.
11. Use `Commit+Push` on the selected repo when you want to stage, commit, and push from it.

Notes:

- The `Codex` launcher starts Codex with `--dangerously-bypass-approvals-and-sandbox` by default.
- If VTE reports a new current directory because you `cd` inside a shell, BelloSaize tracks that and uses it for git actions.
- New panes grab keyboard focus immediately, and a plain click on another pane focuses it without leaving behind accidental text selection.
- Closing the app stops the child processes. BelloSaize does not auto-restore terminals on the next launch.

## Project Discovery

BelloSaize scans a few common roots and fills the project picker from Git repositories it finds there:

- parent of the current working directory
- `~/Documents/github`
- `~/github`
- `~/src`

If nothing is found, it falls back to the current working directory.

## Git Integration

The explorer refresh checks each repository for:

- local dirtiness
- ahead / behind tracking state against its remote
- missing upstream or remote-check failures

`Get Up To Date` runs host-side git commands in the chosen repo scope:

1. `git fetch --all --prune`
2. `git pull --ff-only` when the branch has an upstream, the working tree is clean, and the repo is only behind

If BelloSaize fetches successfully but skips the fast-forward for a repo, it shows the reason in the batch report instead of guessing between merge and rebase behavior.

`Commit+Push` runs host-side git commands in the selected repo. If the focused pane is already inside that repo, BelloSaize uses the pane's tracked directory:

1. `git add -A`
2. `git commit -m ...`
3. `git push`

If the current branch has no upstream yet, BelloSaize falls back to `git push -u origin <branch>`.
If the repo has no uncommitted changes but does have local commits ahead of upstream, BelloSaize skips the commit prompt and only runs the push step.

Progress is shown in the footer status area and active repo status badges with a simple spinner, and the latest transcript is kept on the status tooltip instead of opening a second popup.

## Status

This is an early usable version. It is intentionally narrow in scope:

- Linux only
- GTK4 + VTE only
- mouse-first window management
- no tmux dependency
- no browser frontend

## Open Source

- License: [MIT](LICENSE)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)

## Development

Checks:

```bash
cargo fmt
cargo test
cargo check
```

Release build:

```bash
cargo build --release
```
