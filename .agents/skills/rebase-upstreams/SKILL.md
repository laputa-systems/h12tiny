---
name: rebase-upstreams
description: "Use for rebasing ../h2-futures-lite and ../hyper-futures-lite onto hyperium upstreams, including ../fastwebsockets-futures-lite (no upstream), and updating h12tiny. Temporarily path-overrides h12tiny to the local forks and uses h12tiny as verification."
disable-model-invocation: true
---

# Rebase Upstreams

Rebase the sibling futures-lite forks onto their hyperium upstreams, then update
h12tiny. h12tiny verifies the forks via temporary local path overrides.
Commit nothing. Never push. Never `cargo publish`.

## Scope

- `../h2-futures-lite`: `origin` is hyperium/h2, local `master` tracks the `josh` fork with the futures-lite port stack on top.
- `../hyper-futures-lite`: `origin` is hyperium/hyper, same layout.
- `../fastwebsockets-futures-lite`: no upstream; `origin` is the laputa-systems fork itself (branch `main`). Never rebased, only verified and optionally version-bumped.
- This repo (`h12tiny`): depends on the published `laputa-hyper-futures-lite` (= pin) and `laputa-fastwebsockets-futures-lite` from crates.io; hyper pulls `laputa-h2-futures-lite`.
- Order matters: rebase and settle h2 first, then hyper (hyper pins an exact h2 beta).

## Rules

- **Clean trees or stop.** `git status --porcelain` must be empty in all four repos (h12tiny plus the three siblings) before starting. On any failure, stop and report; leave a conflicted rebase in place for the owner, never abort away resolution work.
- **Discover, do not hardcode.** Find each port stack with `git fetch origin` then `git log --oneline $(git merge-base master origin/master)..master`. Never assume base SHAs or version numbers.
- **Non-interactive rebase.** The harness has no usable editor; run every `git rebase --continue` as `GIT_EDITOR=true git rebase --continue`.
- **Port intent wins conflicts.** Keep the futures-lite adaptations (futures-io/futures-lite runtime neutrality, renamed `laputa-*` packages, non-optional `futures-channel` where the port made it so, gated `tracing`). Take upstream for everything else. Never silently drop a port commit.
- **Version conflicts resolve forward.** Each port commit touching `Cargo.toml` re-conflicts on the version field. Resolve every one to the port package name at `<new-upstream>-beta.0`, preserving the exact `=` pin style.
- **New upstream code on port-replaced types gets adapted.** When upstream adds calls against a type the port replaced (e.g. tokio mpsc `poll_recv` vs futures-channel), rewrite the new code onto the port's adapter keeping upstream's intent, in a fix commit on the fork.
- **Pre-existing red stays red unless trivial.** A fork check that fails identically on the pre-rebase base (confirm via a detached worktree at the old base) is not rebase-caused. Fix it only if it is small and in port-owned code; otherwise report it. Hyper's tokio-based lib tests are known-red this way: `cargo check --features full` is the fork gate, lib-test failures of the same missing-crate kinds as the base are informational.
- **Pinned nightly stays.** Do not change any `rust-toolchain.toml` channel.
- **Local-only mutations.** Committing, pushing, and publishing all require an explicit ask beyond this skill.

## Steps

1. **Baseline.** Record `git log --oneline -3` and `git status -sb` in all four repos, plus the current `laputa-*` pins in this repo's root `Cargo.toml` and each fork's `Cargo.toml`.
2. **Rebase h2.** In `../h2-futures-lite`: `git fetch origin`, rebase the port stack with `git rebase --onto origin/master <merge-base> master`. If upstream released a new tag (check `git tag --sort=-v:refname | head`), bump the fork version to `<upstream>-beta.0`. Run `cargo check` and `cargo test --lib`.
3. **Rebase hyper.** In `../hyper-futures-lite`: `git fetch origin`, same rebase procedure. Update its `h2 = { package = "laputa-h2-futures-lite", version = "=..." }` pin to the settled h2 beta from step 2, bump its own version to `<upstream>-beta.0` on a new upstream tag. Its own checks need the unpublished h2: add a temporary `[patch.crates-io]` for `laputa-h2-futures-lite` pointing at `../h2-futures-lite`, run `cargo check --features full`, then remove the patch before finishing.
4. **Settle fastwebsockets.** In `../fastwebsockets-futures-lite` (branch `main`): no fetch, no rebase, there is no upstream. Run `cargo test --all-features`. Bump its version only if the port itself changed during this run.
5. **Point h12tiny at the new betas, then patch.** Bump this repo's workspace `hyper` pin (root `Cargo.toml`) and the `fastwebsockets` pin (`crates/h12tiny-web/Cargo.toml`, only if step 4 bumped it) to the settled fork versions *before* verifying: with exact `=` pins, a `[patch.crates-io]` entry whose version does not match the requirement is silently ignored. Then append:
   ```toml
   [patch.crates-io]
   laputa-h2-futures-lite = { path = "../h2-futures-lite" }
   laputa-hyper-futures-lite = { path = "../hyper-futures-lite" }
   laputa-fastwebsockets-futures-lite = { path = "../fastwebsockets-futures-lite" }
   ```
   Run `cargo update -p <each laputa crate>` and prove the patch applies: no `was not used in the crate graph` warnings and `cargo tree -i laputa-hyper-futures-lite` shows the local path. If the patch is ignored, stop: the verification that follows would test the old registry crates.
6. **Verify with h12tiny.** With the patch proven active: `cargo test --workspace --all-features` (not `--locked`; the patch intentionally moves the lockfile), then `scripts/check-features.sh` and `sh scripts/check-normal-dependencies.sh`. The `websocket` feature exercises the fastwebsockets fork. Run `make lint` only if steps so far are green. On a hang, first re-check step 5's proof, reproduce the single test solo, and compare against a base worktree at HEAD before suspecting the rebase.
7. **Unpatch, restore, report.** Remove the `[patch.crates-io]` section, keeping the pin bumps. Restore `Cargo.lock` from HEAD (`git checkout -- Cargo.lock`) since the new betas are unpublished and the lock cannot resolve them yet. Confirm no `[patch]` section remains in any of the four repos. Report: port stacks rebased onto which upstream SHAs, version bumps made, checks run with results, and the exact follow-ups (`cargo publish` in each fork bottom-up, then `cargo update -p ...`, then `make test`).

**Reply:** upstream SHAs rebased onto, version bumps, which checks ran and which did not, and the publish/update commands left for the owner.
