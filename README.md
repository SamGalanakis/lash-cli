# lash-cli

The first-party terminal application for the
[Lash agent runtime](https://github.com/SamGalanakis/lash).

The workspace owns the `lash` executable and its private TUI, export, file
index, autoresearch, and test-harness crates. Lash itself remains an embeddable
runtime; this repository is one complete host built on top of it.

## Build

```sh
cargo build --release
```

The executable is written to `target/release/lash`.

## Install

Release binaries are published from this repository:

```sh
curl -fsSL https://github.com/SamGalanakis/lash-cli/releases/latest/download/install_lash.sh | bash
```

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

Lash dependencies are pinned to one exact upstream revision in the workspace
manifest. Update that revision deliberately and verify this entire workspace.

The user-facing command, configuration directory (`~/.lash`), and session
format remain `lash`; splitting the repository does not rename the product or
invalidate existing installations.
