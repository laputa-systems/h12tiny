# Minimum supported Rust version

The fork manifest declares Rust 1.85 because its smolith dependencies require
that compiler. `scripts/check-msrv.sh` checks this fork with Rust 1.85. The
pinned nightly remains the development toolchain. h12tiny's other consumer
compiler requirements are listed in [verification](../../../docs/verification.md).
