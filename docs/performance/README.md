# Performance campaign: HTTP/1.1, HTTP/2, client pool, and router

## Status

**Goal:** remove measurable per-request work from the async server, the
HTTP/2 fork, the pooled client, and the router without changing the
runtime-neutral architecture, the public API, the TLS stack, or the blocking
client.

**Baseline:** commit `b8c59e0`, 2026-11-01. Every result below compares that
tree with the tree at the commit named next to it. The campaign ran on one
macOS host: Apple M5 Max, 18 cores, 48 GB, nightly-2026-09-15, release profile
with thin LTO. Other CPU-heavy work ran on the host during parts of the
campaign; the end-to-end section records when that happened. Both the load
generator and the server run on the host, so absolute throughput is a loopback
number, not a claim about other hardware.

**Accepted:** seven commits after the baseline, each with its own measurement
or a behavior-equivalence argument:

| Commit | Change |
| --- | --- |
| `3b23480` | Reject mixed `Content-Length`/`Transfer-Encoding` and duplicate `Content-Length` framing |
| `c80ae9e` | Decode HPACK header values without copying the `Bytes` payload |
| `0dc407c` | Skip pool clock reads when no idle timeout is configured |
| `84960b2` | Resolve the router method before building path parameters |
| `4646f91` | Poll pooled checkout once before constructing the dial future |
| `169bef3` | Stop pre-sizing the outgoing HTTP/2 header map |
| `4f34ac9` | Build HTTP/2 server requests without `http::request::Builder` |

`3b23480` is a correctness fix that came out of the first CI failure; it has
no performance claim. `f46b68f` adds the repeated matrix script, `ef967ea`
makes a pre-existing flaky client test deterministic, and `c3553df` restores
header-frame equality after the outgoing-constructor change by comparing the
header content rather than a cached size derived from it; none of them change
the measured paths.

## Method

### End-to-end A/B

`examples/interop-server` serves deterministic bodies and a repeated-header
response. `oha` drives HTTP/1.1 and `h2load` drives HTTP/2 with 16 streams per
connection. Each measurement starts a fresh server process and reads its
CPU time from `proc_pid_rusage(RUSAGE_INFO_V4)` before and after the load
generator runs, so the metric is scheduler-independent server CPU per
completed request rather than wall time. macOS reports that field in Mach
absolute ticks; one tick is about 41.7 ns at 24 MHz. Requests per second is
recorded from the load generator for context only.

The campaign script starts every run from a fresh process and swaps the order
of the baseline and candidate across rounds and across the cases inside a
round. Six rounds give the medians below; a three-round same-binary control,
run in the same window, sets the noise floor. The `/headers` endpoint isolates
response encoding; `/64k` includes socket write cost; `/0` isolates request
decode and response framing.

`scripts/bench-matrix.sh` runs the same endpoints and load tools for
reproduction and prints requests per second per cell; it does not sample
server CPU.

### Microbenchmarks

Two hot paths are too small for the end-to-end harness. The pool checkout and
the HPACK decode benchmark loop the operation in one process and discard a
warmup pass. Both harnesses are scratch code that reaches crate-private
types; the committed benchmark surface is `scripts/bench-matrix.sh` and the
`interop-server` examples. Results are medians of repeated timing loops on the
same host, not a stable public benchmark:

| Path | Baseline | After | Change |
| --- | --- | --- | --- |
| HPACK decode, per header | 50.8 ns | 44.7 ns | -12% |
| Pooled H2 checkout | 48.6 ns | 27.8 ns | -43% |

### Client CPU

The pooled client has no deterministic server-side endpoint, so its two
commits were measured with `examples/client-load` sending 2,000,000 HTTP/2
requests through one pooled client against `interop-server`, again with
client CPU from `proc_pid_rusage`. The checkout clock read and the dial
future construction each accounted for about 3-4% of client CPU in that run.
That is a same-run comparison of the client process only; the server and
load generator were not part of the metric.

## Results

### Isolated changes

The HPACK decode and pooled-checkout numbers above come from scratch loops
around crate-private types. They are the only measurements that resolve those
two changes directly. The pooled-client CPU run resolves the checkout and dial
changes, and a same-tree rebuild A/B of the outgoing header constructor
resolves it on the `/headers` workload: about 5% fewer server CPU ticks. The
direct request-construction change is behavior-equivalent in the same
comparison and no separate win is claimed for it.

### End-to-end server CPU

Server CPU ticks per request, median of six rounds at 16 connections, final
tree `c3553df` against baseline `b8c59e0`. One tick is about 41.7 ns.

| Workload | Baseline | Final | Paired median | Paired range |
| --- | --- | --- | --- | --- |
| H1 `/0` | 174.9 | 174.1 | +3.0% | -7.5% to +7.2% |
| H1 `/64k` | 303.1 | 295.5 | -0.7% | -21.1% to +11.6% |
| H2 `/0` | 37.5 | 36.7 | -1.3% | -5.1% to +1.0% |
| H2 `/64k` | 426.0 | 419.6 | -2.2% | -4.2% to +2.4% |
| H2 `/headers` | 75.9 | 74.9 | -1.6% | -6.3% to +3.8% |
| H2 TLS `/0` | 40.7 | 38.7 | -3.9% | -6.2% to -0.5% |

The same-binary control, pairing two runs of the final binary in the same
window, moved by +0.4% to +7.7% per workload (median of pairs) and by up to
10.2% in a single pair. That drift is the floor this harness can resolve. The
H2 cells trend 1-4% lower and the TLS cell was lower in all six rounds, which
is the direction the isolated measurements predict, but the effect is at or
below the control spread. The campaign claims no independently significant
aggregate speedup and no H1 change.

The window started at load 3.0 and ended with `perf-gate-ready -1` exiting 0
at load 4.0 on 18 cores, with unrelated work on the host. Requests per second,
recorded for context, moved by -3.4% to +4.1% across the same cells; the load
generator runs on the same host and is not a capacity signal.

## Rejected

- **H2 write coalescing threshold.** Rewriting the H2 send path to flush after
  256 bytes, and after every frame, both raised server CPU per request versus
  the existing 1024-byte threshold on `/0` and `/headers`. Smaller writes gave
  the runtime more wakeups and more `writev` calls than they saved in buffer
  copying. The threshold stays at 1024.
- **Router dispatch changes beyond the method-first lookup.** After the
  dispatch cleanup, router CPU was inside the end-to-end noise floor on every
  endpoint tried. No further router-specific change was kept; `84960b2`
  stands on readability and on not building path parameters for routes that
  cannot match.
- **`io_uring` or another transport rewrite.** Syscall time is a real share of
  the per-request cost on loopback, but replacing the futures-I/O layer would
  change the runtime-neutral architecture, add platform-specific code, and
  bypass the smolith adapter that the project exists to support. The measured
  per-request overhead did not justify that trade.

## Limitations

- One host, one OS, one runtime (`smol`). The server and the load generator
  share the machine, so requests per second is a loopback ceiling, not a
  capacity claim.
- The changes are individually small. Server CPU per request has a wide
  natural spread on this host because batching, wakeups, and socket writes
  vary from run to run; same-binary control runs spread by more than the
  expected effect. Only the isolated benchmarks and the pooled-client CPU
  metric resolve the individual changes. No aggregate end-to-end speedup is
  claimed.
- TLS results include a fresh handshake per connection; no session resumption
  is exercised.
- The blocking client and the feature matrix were not part of this campaign;
  they are covered by the correctness gates instead.

