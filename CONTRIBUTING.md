# Contributing

Thanks for contributing to BelloSaize.

The project is intentionally narrow: it should stay lightweight, native, and practical for managing multiple AI-agent terminals inside one desktop window. Changes that add a lot of runtime overhead, browser-style complexity, or heavyweight dependencies should clear a high bar.

## Before Opening A PR

1. Keep the scope tight.
2. Prefer simple native solutions over extra layers.
3. Match the existing code style and naming.
4. Update the README when user-facing behavior changes.

## Development Checks

Run these before sending changes:

```bash
cargo fmt
cargo check
cargo test
```

For release verification:

```bash
cargo build --release
```

## Design Direction

If you touch the UI:

- keep it mouse-friendly and low-friction
- avoid adding keyboard-heavy workflows as the default path
- preserve the lightweight native feel
- prefer clarity over decorative complexity

## Commit Style

Use short, direct commit messages that describe the shipped change.

Examples:

- `Add native GTK terminal deck`
- `Replace git actions with commit and push flow`
- `Tighten hero spacing and toolbar layout`

## Bug Reports

When filing an issue, include:

- Linux distro and version
- desktop environment / compositor
- how the app was launched
- what command or project type was running
- screenshots if the problem is visual
