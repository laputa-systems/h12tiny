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

Run `bash benchmarks/twelve-dollar/run.sh box ceiling` before `box search` and
`box confirm`. Select `--mode nginx` or `--mode direct` for each search and
confirmation. A deferred search or confirmation can be resumed with the same
command, revision, reader count, and measured ceiling. Results and search state
live under `.work/twelve-dollar/`.

## Measured configuration

- Reader threads: 2 (`TWELVE_DOLLAR_READERS`). At 500 VUs, runs with 1, 2, and
  4 readers were indistinguishable: 38.0–38.8 req/s and 1.5–1.9 % of the
  1-CPU box, with a 0.47–0.68 ms p50. The executor thread count is logged at
  startup (1 in the box, 18 on this macOS host).
- Generator ceilings (`run.sh box ceiling`, 2026-11-01): nginx 14,000 VUs and
  direct 25,000 VUs, both at the D8 caps rather than generator saturation. k6
  used 130 % CPU and 37.6 % memory at 14,000 VUs, and 353 % CPU and 67.3 %
  memory at 25,000 VUs, with no dropped iterations.
- Run validity (`tools/perf-gate.sh`): the host gate ignores CPU from the
  container runtime itself (`OrbStack Helper`), because that is the benchmark's
  own containers plus their memory reclaim. Any other hot process still defers
  the run, and the load average must stay under the gate limit. The exclusion
  assumes the Docker VM runs only `h12t-twelve-*` containers; `docker ps` showed
  nothing else at the measured gates.

## Loop timings

Measured 2026-11-01 on h12tiny `1c24cdd`, aarch64 macOS, with the debug target
already populated by one full test build.

| Command | Wall time |
| --- | --- |
| `cargo test --workspace --all-features --no-run` (first run) | 22.5 s |
| `cargo test --workspace --all-features --no-run` (warm) | 0.3 s |
| `cargo clippy -p h12tiny-web --all-features` (warm) | 0.9 s |

## Dependencies

The h12tiny server is runtime-neutral, so the app supplies `smol` (`laputa-smol`)
as its executor and timer runtime. `signal-hook` supplies SIGTERM and SIGINT
for the graceful drain. Both are already present in the root dependency graph.

## Layout

- `crates/app-core`: SQLite, JWT, JSON, and validation, with no HTTP.
- `crates/h12tiny-app`: the `h12tiny-twelve` server and the static
  `h12tiny-static` diagnostic server.
- `crates/axum-ref`: the Axum reference server.
- `crates/attribution`: SQLite-only and microbenchmark attribution.
- `run.sh`, `box/`, `tools/`: the local harness.
