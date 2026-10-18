---
name: rebase-upstreams
description: "Rebase the h2 and hyper futures-lite forks onto Hyperium upstream, refresh their monorepo sources, and verify h12tiny against them."
disable-model-invocation: true
---

# Rebase the fork sources

Rebase the Laputa h2 and hyper branches in temporary clones under `.work/tmp`,
then copy the updated sources into `crates/`. h12tiny uses local path
dependencies to these sources. Do not commit the monorepo, push, or publish.

## Source layout

- `crates/laputa-h2-futures-lite`: `laputa-h2-futures-lite`
- `crates/laputa-hyper-futures-lite`: `laputa-hyper-futures-lite`
- `crates/laputa-fastwebsockets-futures-lite`: `laputa-fastwebsockets-futures-lite`

The package directories have no nested `.git` directories. Temporary clones hold
the h2 and hyper branch histories and upstream remotes.

## Rules

- Stop unless the monorepo working tree is clean.
- Put clones and temporary files under `.work/tmp`. Ensure `/.work/` appears in
  `.git/info/exclude`, then set `TMPDIR` to `$ROOT/.work/tmp`.
- Rebase h2 before hyper. Hyper pins an exact h2 beta.
- Copy each current monorepo source snapshot into its temporary clone before
  rebasing. Commit snapshot changes in the temporary clone so monorepo edits
  survive the rebase.
- Resolve conflicts in favor of the futures-lite port for `futures-io`,
  `futures-lite`, renamed `laputa-*` packages, non-optional `futures-channel`,
  and gated `tracing`. Keep upstream changes elsewhere.
- Resolve port commits that change a package version to `<upstream>-beta.0`.
  Keep exact `=` requirements on fork dependencies.
- Keep every `rust-toolchain.toml` channel unchanged.
- Leave a conflicted temporary clone in `.work/tmp`. Do not abort away conflict
  resolution work.

## Rebase and install

1. From the monorepo root, set `ROOT=$(git rev-parse --show-toplevel)`. Record
   `git log --oneline -3`, `git status -sb`, the three fork versions, and the
   dependency version requirements in the root and fork manifests.
2. Confirm `.work/` is ignored through `.git/info/exclude`. Create
   `$ROOT/.work/tmp/upstreams` and export `TMPDIR="$ROOT/.work/tmp"`.
3. Clone h2 and overlay the current source snapshot:

   ```sh
   git clone --branch master git@github.com:laputa-systems/h2-futures-lite.git "$TMPDIR/upstreams/laputa-h2-futures-lite"
   rsync -a --delete --exclude='/.git/' --exclude='/Cargo.lock' "$ROOT/crates/laputa-h2-futures-lite/" "$TMPDIR/upstreams/laputa-h2-futures-lite/"
   git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" add --all
   ```

   Commit the staged snapshot only when it differs from the clone's HEAD.
   Use `chore: sync monorepo fork snapshot` for that temporary commit.
4. Add and fetch the h2 upstream, then record the port stack:

   ```sh
   git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" remote add upstream https://github.com/hyperium/h2.git
   git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" fetch upstream --tags
   git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" log --oneline "$(git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" merge-base master upstream/master)..master"
   ```

   Set and use the base in the temporary clone:

   ```sh
   base=$(git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" merge-base master upstream/master)
   git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" rebase --onto upstream/master "$base" master
   ```

   After resolving a conflict, continue with
   `GIT_EDITOR=true git rebase --continue` from the clone. Check its newest
   upstream tags with `git -C "$TMPDIR/upstreams/laputa-h2-futures-lite" tag
   --sort=-v:refname | head`. Set the h2 version to `<upstream>-beta.0` when
   Hyperium has released a new tag.
5. Copy the rebased h2 sources back to the repository:

   ```sh
   rsync -a --delete --exclude='/.git/' --exclude='/Cargo.lock' "$TMPDIR/upstreams/laputa-h2-futures-lite/" "$ROOT/crates/laputa-h2-futures-lite/"
   ```

   Run the h2 checks from the monorepo root:

   ```sh
   cargo check --manifest-path crates/laputa-h2-futures-lite/Cargo.toml
   cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --lib
   ```

6. Set Hyper's exact h2 version in `crates/laputa-hyper-futures-lite/Cargo.toml` to the
   settled h2 beta. Keep its local path `../laputa-h2-futures-lite`. Clone hyper,
   overlay the current monorepo snapshot, and commit staged snapshot changes in
   the temporary clone:

   ```sh
   git clone --branch master git@github.com:laputa-systems/hyper-futures-lite.git "$TMPDIR/upstreams/laputa-hyper-futures-lite"
   rsync -a --delete --exclude='/.git/' --exclude='/Cargo.lock' "$ROOT/crates/laputa-hyper-futures-lite/" "$TMPDIR/upstreams/laputa-hyper-futures-lite/"
   git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" add --all
   ```

7. Add and fetch the Hyperium upstream:

   ```sh
   git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" remote add upstream https://github.com/hyperium/hyper.git
   git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" fetch upstream --tags
   git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" log --oneline "$(git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" merge-base master upstream/master)..master"
   ```

   Set the base and rebase Hyper:

   ```sh
   base=$(git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" merge-base master upstream/master)
   git -C "$TMPDIR/upstreams/laputa-hyper-futures-lite" rebase --onto upstream/master "$base" master
   ```

   Continue conflicts with `GIT_EDITOR=true git rebase --continue` from the
   Hyper clone. If Hyperium has a new tag, set Hyper's version to
   `<upstream>-beta.0`. Keep its exact h2 beta pin and local path. Hyper's h2
   path resolves because both clones are under `$TMPDIR/upstreams/`.
8. Copy the rebased Hyper sources back to the repository:

   ```sh
   rsync -a --delete --exclude='/.git/' --exclude='/Cargo.lock' "$TMPDIR/upstreams/laputa-hyper-futures-lite/" "$ROOT/crates/laputa-hyper-futures-lite/"
   ```

   Then run:

   ```sh
   cargo check --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full
   ```

   Hyper's Tokio-based library tests are not a gate. `cargo check --features
   full` is the Hyper gate. Compare any other failure with the fork branch
   before the rebase.
9. Fastwebsockets has no upstream. Run `cargo test --all-features` from
   `crates/laputa-fastwebsockets-futures-lite`. Bump its version only when its source
   changes.
10. Update the root Hyper pin in `Cargo.toml` and the fastwebsockets pin in
    `crates/h12tiny-web/Cargo.toml` when their package versions changed. Run
    these commands from the monorepo root:

    ```sh
    cargo update -p laputa-h2-futures-lite
    cargo update -p laputa-hyper-futures-lite
    cargo update -p laputa-fastwebsockets-futures-lite
    ```

    Confirm the package paths from the monorepo root:

    ```sh
    cargo tree -i laputa-h2-futures-lite
    cargo tree -i laputa-hyper-futures-lite
    cargo tree --features websocket -i laputa-fastwebsockets-futures-lite
    ```
11. Run `cargo test --workspace --all-features`, `scripts/check-features.sh`,
    and `sh scripts/check-normal-dependencies.sh`. Run `make lint` when those
    checks pass. The `websocket` feature exercises the fastwebsockets fork.
12. Keep the updated root `Cargo.lock`; it records local path dependencies.
    Confirm no `[patch]` section remains in the root or fork manifests. Remove
    successful temporary clones. Leave conflicted clones in place.

Report the upstream SHAs, package version changes, checks, and any follow-up
publication needed for the new versions.
