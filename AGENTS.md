# Repository workflow

lash-cli uses trunk-based development with `main` as the only long-lived
branch.

- Start changes from an up-to-date `main` on a short-lived branch.
- Ship through a pull request; do not push product changes directly to `main`.
- Keep pull requests focused and merge only after required CI checks pass.
- Releases are manual through the `Release` workflow for a green commit on
  `main`; do not create tags or publish artifacts by hand.
- Include a `Release-Notes:` section in each releasable commit range.

This repository owns the `lash` executable and its private support crates. The
Lash repository owns the reusable runtime and SDK. Do not add unpublished path
dependencies back across that boundary; update the exact Lash revision in the
root manifest deliberately.
