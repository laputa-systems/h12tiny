---
name: publish-forks
description: "Use for publishing the rebased futures-lite forks and h12tiny to crates.io in dependency order: h2, hyper, fastwebsockets, then h12tiny. Runs after rebase-upstreams."
disable-model-invocation: true
---

# Publish Forks

Publish the rebased forks and h12tiny to crates.io, in dependency order.
Publishing and pushing are outward-facing: run them only on an explicit ask
in this session naming the crates. Stop for that confirmation after dry-runs.

## Order

1. `../h2-futures-lite` (`laputa-h2-futures-lite`)
2. `../hyper-futures-lite` (`laputa-hyper-futures-lite`, pins the h2 beta)
3. `../fastwebsockets-futures-lite` (`laputa-fastwebsockets-futures-lite`)
4. This repo (`h12tiny` workspace, pins the hyper beta)

## Rules

- **rebase-upstreams first.** Each crate's verification from that skill must be green. Never publish over red checks.
- **No `[patch]` sections.** `grep -rn "\[patch" --include=Cargo.toml` across all four repos must be empty before any publish.
- **Skip unchanged crates.** If the local version is already on crates.io (publish reports already-uploaded, and the tree has no changes since that release), skip it and continue downstream. Do not republish.
- **Pinned nightly stays.** Do not change any `rust-toolchain.toml` channel.

## Steps

1. **Confirm scope.** Clean trees everywhere, versions as settled by the rebase
   (h2 `<upstream>-beta.0`, hyper `<upstream>-beta.0`, fastwebsockets only if
   the port changed), downstream pins already bumped. Run
   `cargo publish --dry-run` per changed crate in order. Then stop and ask the
   owner to confirm the publish list.
2. **Publish bottom-up.** For each confirmed crate in order: `cargo publish`,
   then confirm the version resolves from crates.io (`cargo update -p <pkg>
   --precise <version> --dry-run` from the downstream repo, retrying on index
   lag) before moving up the stack. Do not rely on pushing the branch: the
   fork pre-push hook refuses non-terminal pushes, so report each published
   crate's unpushed branch and leave `git push` to the owner.
3. **Update h12tiny.** Remove nothing; set the workspace `hyper` pin (root
   `Cargo.toml`) and the `fastwebsockets` pin (`crates/h12tiny-web/Cargo.toml`,
   only if republished) to the published versions, run `cargo update -p ...`
   for real, and verify with `make test` (`cargo test --locked --workspace
   --all-features`). Then `scripts/check-features.sh` and
   `sh scripts/check-normal-dependencies.sh`.
4. **Release h12tiny** only if asked: `make publish` bumps workspace minor
   versions, tags, and publishes crates in dependency order. That is a
   separate decision from publishing the forks.

**Reply:** what was published (versions, order), what was skipped and why,
downstream pin/lock updates, and check results.
