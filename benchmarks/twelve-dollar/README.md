# Twelve Dollar Server Challenge, measured locally

A contained benchmark application for the
[Twelve Dollar Server Challenge](https://github.com/arjaythedev/twelve-dollar-server-challenge),
built on h12tiny's server, router, and body utilities. Every result is measured
on this machine; nothing is submitted, and the numbers are not comparable to
the leaderboard.

## Setup

Requirements: Rust from the pinned `rust-toolchain.toml`, Node.js 18 or newer,
`sqlite3`, Docker, and `jaq`.

```sh
bash benchmarks/twelve-dollar/setup.sh
```

`setup.sh` clones the challenge at its pinned revision into
`.work/twelve-dollar/challenge`, builds `seed/feed.db`, and asserts that the
checkout is still clean. It never refreshes an existing clone.

## Loop timings

Measured 2026-11-01 on h12tiny `1c24cdd`, aarch64 macOS, with the debug target
already populated by one full test build.

| Command | Wall time |
| --- | --- |
| `cargo test --workspace --all-features --no-run` (first run) | 22.5 s |
| `cargo test --workspace --all-features --no-run` (warm) | 0.3 s |
| `cargo clippy -p h12tiny-web --all-features` (warm) | 0.9 s |

## Layout

- `crates/app-core`: SQLite, JWT, JSON, and validation, with no HTTP.
- `crates/h12tiny-app`: the `h12tiny-twelve` server and the static
  `h12tiny-static` diagnostic server.
- `crates/axum-ref`: the Axum reference server.
- `crates/attribution`: SQLite-only and microbenchmark attribution.
- `run.sh`, `box/`, `tools/`: the local harness.
