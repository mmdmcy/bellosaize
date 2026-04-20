# BelloSaize

BelloSaize is a lightweight native Linux terminal deck for running multiple Codex, Claude, Mistral, and shell sessions in one clickable window.

It is built for the workflow where the heavy work already happens inside the agent terminals, so the harness itself stays simple:

- real embedded terminals via GTK4 + VTE
- automatic tiling for 1, 2, 3, 4, and larger pane counts
- project-aware launcher for repositories under your usual code roots
- click-to-focus panes, double-click-to-zoom headers
- focused-pane `Commit+Push` flow with one prompt
- session persistence in `~/.config/bellosaize/session.toml`

This is a normal desktop app. It does not run a browser UI, it does not need `localhost`, and it does not depend on Electron.

## Why

Running several AI coding agents at once gets messy fast:

- terminals drift across workspaces
- it becomes hard to tell which pane belongs to which repo
- switching between sessions takes more mental overhead than it should

BelloSaize keeps those sessions inside one native window, shows the project for each pane, and gives you one place to launch and manage them.

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

1. Pick a repository from the `Project` dropdown.
2. Launch a pane with `New Shell`, `New Codex`, `New Claude`, `New Mistral`, or `Custom...`.
3. Click any pane to focus it.
4. Double-click a pane header to zoom or unzoom it.
5. Use `Commit+Push` on the focused pane when you want to stage, commit, and push from that repository.

Notes:

- If VTE reports a new current directory because you `cd` inside a shell, BelloSaize tracks that and uses it for git actions.
- Closing the app stops the child processes and saves the pane specs so the next launch can restore the layout.

## Project Discovery

BelloSaize scans a few common roots and fills the project picker from Git repositories it finds there:

- parent of the current working directory
- `~/Documents/github`
- `~/github`
- `~/src`

If nothing is found, it falls back to the current working directory.

## Git Integration

`Commit+Push` runs host-side git commands in the focused pane's tracked directory:

1. `git add -A`
2. `git commit -m ...`
3. `git push`

If the current branch has no upstream yet, BelloSaize falls back to `git push -u origin <branch>`.

Output is shown in a dialog instead of being injected into the running terminal process.

## Persistence

Open pane specs are saved to:

```text
~/.config/bellosaize/session.toml
```

That file stores:

- project path
- command
- optional pane name
- launcher profile

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
