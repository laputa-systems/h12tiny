---
name: publish-forks
description: "Publish the fork packages from the h12tiny monorepo in dependency order, then release h12tiny when separately requested."
disable-model-invocation: true
---

# Publish the fork packages

Publish the fork packages from their manifests under `crates/`. Publishing and
pushing are outward-facing. Do them only when the owner explicitly names the
crates to publish.

## Order

1. `crates/laputa-h2-futures-lite` (`laputa-h2-futures-lite`)
2. `crates/laputa-hyper-futures-lite` (`laputa-hyper-futures-lite`)
3. `crates/laputa-fastwebsockets-futures-lite` (`laputa-fastwebsockets-futures-lite`)
4. The h12tiny workspace, only when separately requested

## Rules

- Complete the [upstream rebase workflow](../rebase-upstreams/SKILL.md) first.
- Require a clean monorepo tree before a dry run or publish.
- Run commands from the monorepo root and set `ROOT=$(git rev-parse --show-toplevel)`.
- Ensure `/.work/` appears in `.git/info/exclude` before creating `.work/tmp`.
  Set `TMPDIR` to `$ROOT/.work/tmp` for Cargo commands.
- Keep the root and fork manifests free of `[patch.crates-io]` sections.
- Publish only changed fork packages. Skip a version already on crates.io when
  its source has not changed since release.
- The fork packages are excluded from the h12tiny Cargo workspace. Use
  `--manifest-path` for every fork package command.
- Do not push the monorepo or a fork branch unless the owner asks.
- Keep every `rust-toolchain.toml` channel unchanged.

## Dry run

Run `cargo publish --manifest-path <manifest> --dry-run` for each changed fork
package in dependency order. Then report the exact package names and versions
and wait for the owner's explicit publish request.

## Publish

For each confirmed package, run `cargo publish --manifest-path <manifest>` in
dependency order. Verify the published version with
`cargo info <package>@<version>` before moving to its dependent package. Retry
after registry index lag when needed.

After publishing the fork packages, the root `Cargo.lock` continues to point
at local paths for development. The package manifests keep the published
version requirements; Cargo omits the local `path` fields when it packages a
crate.

Run `make publish` only when the owner separately asks to release h12tiny. It
bumps, commits, tags, and publishes only h12tiny workspace crates. It does not
publish the fork packages.

Report the packages and versions published or skipped, registry checks, and
any h12tiny release decision that remains.
