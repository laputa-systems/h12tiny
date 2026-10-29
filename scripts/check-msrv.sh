#!/usr/bin/env sh
# Check each component with the Rust version declared by its own manifest.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
installed_toolchains=''

declared_rust_version() {
    awk '
        $1 == "rust-version" && $2 == "=" {
            gsub(/"/, "", $3)
            print $3
            found = 1
            exit
        }
        END { if (!found) exit 1 }
    ' "$1"
}

ensure_toolchain() {
    toolchain=$1
    case " $installed_toolchains " in
        *" $toolchain "*) ;;
        *)
            rustup toolchain install "$toolchain" --profile minimal
            installed_toolchains="$installed_toolchains $toolchain"
            ;;
    esac
}

check() {
    manifest=$1
    shift
    version=$(declared_rust_version "$manifest")
    ensure_toolchain "$version.0"
    cargo "+$version.0" check --locked --manifest-path "$manifest" "$@"
}

# The forks are excluded from the workspace and do not keep a lockfile.
check_fork() {
    manifest=$1
    shift
    version=$(declared_rust_version "$manifest")
    ensure_toolchain "$version.0"
    cargo "+$version.0" check --manifest-path "$manifest" "$@"
}

# These components still declare Rust 1.85 as their minimum supported version.
check crates/h12tiny-core/Cargo.toml
check crates/h12tiny-client-normalize/Cargo.toml
check crates/h12tiny-util/Cargo.toml --all-features

# These components declare Rust 1.89, so both their plain and TLS feature
# selections are checked with that compiler.
check crates/h12tiny-client/Cargo.toml --no-default-features --features http1,http2
check crates/h12tiny-client-sync/Cargo.toml --no-default-features
check crates/h12tiny-server/Cargo.toml --no-default-features --features http1,http2
check crates/h12tiny-tls-system/Cargo.toml

# The facade and WebSocket-enabled web component also declare Rust 1.89.
check Cargo.toml --workspace --all-features
check crates/h12tiny-client/Cargo.toml --no-default-features --features http1,http2,tls
check crates/h12tiny-client-sync/Cargo.toml --no-default-features --features tls
check crates/h12tiny-server/Cargo.toml --no-default-features --features http1,http2,tls
check crates/h12tiny-web/Cargo.toml --all-features

check_fork crates/laputa-h2-futures-lite/Cargo.toml -p laputa-h2-futures-lite
check_fork crates/laputa-hyper-futures-lite/Cargo.toml --features full
check_fork crates/laputa-fastwebsockets-futures-lite/Cargo.toml

printf '%s\n' 'minimum supported Rust version checks passed'
