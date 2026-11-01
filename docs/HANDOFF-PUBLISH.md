# Handoff: finish the 0.7.0 release

State as of 2026-11-01; the last functional change was `ef3f34e`. No crates
were published and no release commit or tag exists yet.

## Ready

- `make publish` targets crates.io despite the repository source replacement.
  Registry queries pass `--registry crates-io`; publish verification runs from
  `/` with the repo toolchain.
- The fork publish guide uses the same bypass.
- Cargo login, the mycrates state and token in
  `~/.local/state/stuff/mycrates`, and Cloudflare auth are present.
- From this checkout, `../stuff` resolves to `/Users/josh/d/stuff`.
- The untracked `.cloudflare/` cache is in this checkout's
  `.git/info/exclude`. Confirm `git status --porcelain` is empty before
  starting; `make publish` requires a clean tree.

## Publish forks first

`make publish` cannot resolve `laputa-hyper-futures-lite =1.12.0-beta.1` until
that version is on crates.io.

| crate | local | crates.io |
| --- | --- | --- |
| `laputa-h2-futures-lite` | 0.4.20-beta.1 | 0.4.20-beta.1, published 2026-10-30 |
| `laputa-hyper-futures-lite` | 1.12.0-beta.1 | 1.12.0-beta.0 |
| `laputa-fastwebsockets-futures-lite` | 0.10.1 | 0.10.0 |

The H2 source changed after 0.4.20-beta.1 was published (`c9449e0`,
`c80ae9e`, `169bef3`, `4f34ac9`, `c3553df`). Choose one:

- Recommended: bump H2 to `0.4.20-beta.2`, update the exact pin in
  `crates/laputa-hyper-futures-lite/Cargo.toml`, the H2 README example, and
  the fork versions in `docs/verification.md`, run
  `sh scripts/verify-protocols.sh`, commit, then publish H2.
- Skip H2: publish Hyper against the existing 0.4.20-beta.1. The published
  Hyper then does not carry the H2 changes made after 2026-10-30.

Publish from `/`, dry-running first:

```sh
ROOT=/Users/josh/d/h12tiny
TOOLCHAIN=$(cd "$ROOT" && rustup show active-toolchain | awk '{print $1}')
cd /
RUSTUP_TOOLCHAIN=$TOOLCHAIN cargo publish --manifest-path "$ROOT/crates/laputa-h2-futures-lite/Cargo.toml" --dry-run --registry crates-io
RUSTUP_TOOLCHAIN=$TOOLCHAIN cargo publish --manifest-path "$ROOT/crates/laputa-h2-futures-lite/Cargo.toml" --registry crates-io
```

Repeat for `laputa-hyper-futures-lite`, then
`laputa-fastwebsockets-futures-lite`. Verify each from `/` before moving on:

```sh
cargo info <package>@<version> --registry crates-io
```

## Release h12tiny

From `/Users/josh/d/h12tiny`:

```sh
cd /Users/josh/d/h12tiny
TMPDIR="$PWD/.work/tmp" make publish
```

This bumps the nine workspace crates from 0.6.0 to 0.7.0, commits and tags
one release, publishes in dependency order, then runs
`make publish-mycrates` for the four crates listed in the Makefile.

If it stops after the release commit, rerun the same command; it finishes
unpublished crates instead of bumping again. If only the mycrates half fails,
run `make publish-mycrates`.

The script does not push. After it finishes:

```sh
git push --follow-tags origin main
git tag --points-at HEAD
```

## Verify

- `cargo info h12tiny@0.7.0 --registry crates-io` from `/`, and repeat for
  each workspace crate.
- `git tag --points-at HEAD` lists the nine release tags, and `origin/main`
  matches.
- `publish-mycrates` reports the site generation and R2 sync.

Related, already pushed: the same source-replacement fix landed in hi-lite
(`046f770`), ptytest (`16abafd`), and rustybench (`84f51c3`).

Delete this file once the release and tags are pushed.
