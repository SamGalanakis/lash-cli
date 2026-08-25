# lash-cli

The first-party terminal application for the
[Lash agent runtime](https://github.com/SamGalanakis/lash).

The workspace owns the `lash` executable and its private TUI, export, file
index, autoresearch, and test-harness crates. Lash itself remains an embeddable
runtime; this repository is one complete host built on top of it.

Documentation: [CLI reference](https://samgalanakis.github.io/lash-cli/) ·
[architecture](https://samgalanakis.github.io/lash-cli/architecture.html) ·
[publishing](https://samgalanakis.github.io/lash-cli/publishing.html)

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

Lash dependencies are pinned to one exact commit on Lash `main`, never
to a release tag. Update that revision deliberately and verify this entire workspace.

The user-facing command and configuration directory (`~/.lash`) remain `lash`.
Sessions are discovered from the unified Lash catalog; CLI sidecars supply only
display metadata. The pinned durable compatibility markers are session schema
41, trace schema 9, and remote protocol 46. Alpha-era per-session `.db` files
are diagnosed as incompatible and refused rather than opened.
