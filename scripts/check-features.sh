#!/usr/bin/env sh
# Verify the component feature graph as a product contract. Cargo.lock is not
# inspected: optional and dev dependencies may legitimately appear there.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

tree() {
    cargo tree "$@" -e normal --prefix none
}

has_package() {
    package=$1
    graph=$2
    printf '%s\n' "$graph" | awk -v package="$package" '$1 == package { found = 1 } END { exit !found }'
}

require_absent() {
    package=$1
    graph=$2
    label=$3
    if has_package "$package" "$graph"; then
        printf '%s unexpectedly contains %s\n' "$label" "$package" >&2
        exit 1
    fi
}

require_present() {
    package=$1
    graph=$2
    label=$3
    if ! has_package "$package" "$graph"; then
        printf '%s is missing expected %s\n' "$label" "$package" >&2
        exit 1
    fi
}

check_forbidden() {
    graph=$1
    label=$2
    for package in tokio tokio-util native-tls ring aws-lc-rs aws-lc-sys openssl openssl-sys cc cmake bindgen reqwest axum hyper-util tower tower-layer webpki-roots; do
        require_absent "$package" "$graph" "$label"
    done
}

client_h1=$(tree -p h12tiny-client --no-default-features --features http1)
client_h2=$(tree -p h12tiny-client --no-default-features --features http2)
client_h1_tls=$(tree -p h12tiny-client --no-default-features --features http1,tls)
sync_client_plain=$(tree -p h12tiny-client-sync --no-default-features)
sync_client_tls=$(tree -p h12tiny-client-sync --no-default-features --features tls)
server_h1_tls=$(tree -p h12tiny-server --no-default-features --features http1,tls)
server_h1=$(tree -p h12tiny-server --no-default-features --features http1)
server_h2=$(tree -p h12tiny-server --no-default-features --features http2)
util_plain=$(tree -p h12tiny-util --no-default-features)
web_plain=$(tree -p h12tiny-web --no-default-features)
web_websocket=$(tree -p h12tiny-web --no-default-features --features websocket)
facade_h1=$(tree -p h12tiny --no-default-features --features client,http1)
facade_sync_tls=$(tree -p h12tiny --no-default-features --features client-sync,tls)
facade_websocket=$(tree -p h12tiny --no-default-features --features websocket)

for package in laputa-h2-futures-lite h12tiny-server h12tiny-web rustls h12tiny-tls-system rustls-native-certs webpki-roots serde serde_json tokio; do
    require_absent "$package" "$client_h1" "H1-only client"
done
require_present laputa-async-net "$client_h1" "H1-only client"
require_present laputa-h2-futures-lite "$client_h2" "H2-only client"
require_present laputa-async-net "$client_h2" "H2-only client"
for package in h12tiny-server h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$client_h2" "H2-only client"
done
require_present rustls "$client_h1_tls" "TLS H1 client"
require_present rustls-graviola "$client_h1_tls" "TLS H1 client"
require_present graviola "$client_h1_tls" "TLS H1 client"
require_present laputa-async-net "$client_h1_tls" "TLS H1 client"
require_present h12tiny-tls-system "$client_h1_tls" "TLS H1 client"
require_present rustls-native-certs "$client_h1_tls" "TLS H1 client"
require_present rustls-webpki "$client_h1_tls" "TLS H1 client"
require_absent webpki-roots "$client_h1_tls" "TLS H1 client"
for package in async-io laputa-async-io async-net laputa-async-net futures-channel futures-core futures-io futures-lite laputa-futures-lite futures-rustls futures-util laputa-hyper-futures-lite laputa-h2-futures-lite h12tiny-core h12tiny-tls-system rustls-native-certs webpki-roots rustls serde serde_json tokio; do
    require_absent "$package" "$sync_client_plain" "plain sync client"
done
require_present rustls "$sync_client_tls" "TLS sync client"
require_present rustls-graviola "$sync_client_tls" "TLS sync client"
require_present graviola "$sync_client_tls" "TLS sync client"
require_present h12tiny-tls-system "$sync_client_tls" "TLS sync client"
require_present rustls-native-certs "$sync_client_tls" "TLS sync client"
require_present rustls-webpki "$sync_client_tls" "TLS sync client"
require_absent webpki-roots "$sync_client_tls" "TLS sync client"
for package in async-io laputa-async-io async-net laputa-async-net futures-channel futures-core futures-io futures-lite laputa-futures-lite futures-rustls futures-util laputa-hyper-futures-lite laputa-h2-futures-lite h12tiny-core serde serde_json tokio; do
    require_absent "$package" "$sync_client_tls" "TLS sync client"
done
require_present rustls "$server_h1_tls" "TLS H1 server"
require_present rustls-graviola "$server_h1_tls" "TLS H1 server"
require_present graviola "$server_h1_tls" "TLS H1 server"
require_absent h12tiny-tls-system "$server_h1_tls" "TLS H1 server"
require_absent rustls-native-certs "$server_h1_tls" "TLS H1 server"
require_absent webpki-roots "$server_h1_tls" "TLS H1 server"

for package in laputa-h2-futures-lite h12tiny-client h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$server_h1" "H1-only server"
done
require_present laputa-h2-futures-lite "$server_h2" "H2-only server"
for package in h12tiny-client h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$server_h2" "H2-only server"
done

for package in serde serde_json tokio h12tiny-client h12tiny-server h12tiny-web; do
    require_absent "$package" "$util_plain" "util without json"
done
for package in base64 laputa-fastwebsockets-futures-lite h12tiny-core serde_json serde_urlencoded sha1 tokio tower axum; do
    require_absent "$package" "$web_plain" "web without json/query"
done
for package in base64 laputa-fastwebsockets-futures-lite h12tiny-core h12tiny-server sha1; do
    require_present "$package" "$web_websocket" "web WebSocket feature"
done
for package in base64 laputa-fastwebsockets-futures-lite sha1; do
    require_present "$package" "$facade_websocket" "facade WebSocket feature"
done
for package in laputa-h2-futures-lite h12tiny-server h12tiny-web h12tiny-tls-system rustls-native-certs webpki-roots rustls serde serde_json tokio; do
    require_absent "$package" "$facade_h1" "facade H1-only client"
done
require_present laputa-async-net "$facade_h1" "facade H1-only client"
require_present h12tiny-client-sync "$facade_sync_tls" "facade TLS sync client"
require_present rustls "$facade_sync_tls" "facade TLS sync client"
require_present h12tiny-tls-system "$facade_sync_tls" "facade TLS sync client"
require_absent webpki-roots "$facade_sync_tls" "facade TLS sync client"
for package in async-io laputa-async-io async-net laputa-async-net futures-channel futures-core futures-io futures-lite laputa-futures-lite futures-rustls futures-util laputa-hyper-futures-lite laputa-h2-futures-lite h12tiny-client h12tiny-core h12tiny-server h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$facade_sync_tls" "facade TLS sync client"
done

for label_and_graph in \
    "H1-only client:$client_h1" \
    "H2-only client:$client_h2" \
    "TLS H1 client:$client_h1_tls" \
    "plain sync client:$sync_client_plain" \
    "TLS sync client:$sync_client_tls" \
    "TLS H1 server:$server_h1_tls" \
    "H1-only server:$server_h1" \
    "H2-only server:$server_h2" \
    "util without json:$util_plain" \
    "web without json/query:$web_plain" \
    "web WebSocket feature:$web_websocket" \
    "facade H1-only client:$facade_h1" \
    "facade TLS sync client:$facade_sync_tls" \
    "facade WebSocket feature:$facade_websocket"; do
    label=${label_and_graph%%:*}
    graph=${label_and_graph#*:}
    check_forbidden "$graph" "$label"
done

cargo check -p h12tiny-client --no-default-features --features http1
cargo check -p h12tiny-client --no-default-features --features http2
cargo check -p h12tiny-client --no-default-features --features http1,http2
cargo check -p h12tiny-client --no-default-features --features http1,tls
cargo check -p h12tiny-client --no-default-features --features http2,tls
cargo check -p h12tiny-client-sync --no-default-features
cargo check -p h12tiny-client-sync --no-default-features --features tls
cargo check -p h12tiny-server --no-default-features --features http1
cargo check -p h12tiny-server --no-default-features --features http2
cargo check -p h12tiny-server --no-default-features --features http1,http2
cargo check -p h12tiny-server --no-default-features --features http1,tls
cargo check -p h12tiny-server --no-default-features --features http2,tls
cargo check -p h12tiny --no-default-features --features client,http1
cargo check -p h12tiny --no-default-features --features client,http2
cargo check -p h12tiny --no-default-features --features client-sync,tls
cargo check -p h12tiny --no-default-features --features server,http1
cargo check -p h12tiny --no-default-features --features server,http2
cargo check -p h12tiny --no-default-features --features websocket
cargo check -p h12tiny --no-default-features --features client,server,http1,http2,tls
cargo check -p h12tiny --no-default-features --features full

# Single-protocol defaults are compiled out of the all-features test suite.
cargo test --locked -p h12tiny --no-default-features --features client,server,http1,tls --test dogfood_tls_h1_only
cargo test --locked -p h12tiny --no-default-features --features client,server,http2 --test dogfood_h2_prior h2_only_build_default_uses_cleartext_prior_knowledge

printf '%s\n' 'feature and dependency isolation checks passed'
