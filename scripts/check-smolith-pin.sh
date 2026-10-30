#!/bin/sh
# H12T-10: laputa-* runtime deps must track the smolith workspace release.
# (a) SMOLITH_REV is a 40-char hex hash (bare rev, no branch/tag prefix).
# (b) Every laputa-* requirement in the root manifest and the three fork
#     manifests resolves to one consistent version (currently 0.3.0);
#     any 0.2.0 remainder or root/fork mismatch fails.
# (c) When a ../smolith checkout sits next to this repo, its workspace
#     version must equal the required laputa-* version.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
EXPECTED="0.3.0"

rev=$(tr -d '[:space:]' < "$ROOT/SMOLITH_REV")
case "$rev" in
  *[!0-9a-f]* | "" )
    echo "check-smolith-pin: SMOLITH_REV is not lowercase hex: '$rev'" >&2
    exit 1
    ;;
esac
if [ "${#rev}" -ne 40 ]; then
  echo "check-smolith-pin: SMOLITH_REV must be a 40-char hash, got ${#rev} chars: '$rev'" >&2
  exit 1
fi

mismatch=0
for m in "$ROOT/Cargo.toml" \
    "$ROOT/crates/h12tiny-client/Cargo.toml" \
    "$ROOT/crates/laputa-h2-futures-lite/Cargo.toml" \
    "$ROOT/crates/laputa-hyper-futures-lite/Cargo.toml" \
    "$ROOT/crates/laputa-fastwebsockets-futures-lite/Cargo.toml" \
    "$ROOT/crates/h12tiny-web/Cargo.toml"; do
  # Inspect each laputa-* requirement line, skipping exact (=x.y.z) pins
  # and the fork crates themselves (laputa-h2/hyper/fastwebsockets):
  # those pin the forks, not the laputa runtime deps.
  while IFS= read -r line; do
    pkg=$(printf '%s' "$line" | grep -o 'package = "laputa-[a-z-]*"' | grep -o '"laputa-[a-z-]*"' | tr -d '"' || true)
    [ -z "${pkg:-}" ] && continue
    case "$pkg" in
      laputa-h2-futures-lite | laputa-hyper-futures-lite | laputa-fastwebsockets-futures-lite)
        continue
        ;;
    esac
    v=$(printf '%s' "$line" | grep -o 'version = "[^"]*"' | head -1 | grep -o '"[^"]*"' | tr -d '"')
    case "$v" in
      =*) continue ;;
    esac
    if [ "$v" != "$EXPECTED" ]; then
      echo "check-smolith-pin: $m requires $pkg $v, expected $EXPECTED" >&2
      mismatch=1
    fi
  done <<EOF
$(grep 'laputa-' "$m" || true)
EOF
  if grep -q 'laputa-.*"0\.2\.0"' "$m"; then
    echo "check-smolith-pin: $m still references laputa-* 0.2.0" >&2
    mismatch=1
  fi
done
if [ "$mismatch" -ne 0 ]; then
  exit 1
fi

SMOLITH="$ROOT/../smolith/Cargo.toml"
if [ -f "$SMOLITH" ]; then
  ws_version=$(awk '/^\[workspace\]/{w=1} w && /version *= *"[0-9]/ {gsub(/.*"/,""); gsub(/".*/,""); print; exit}' "$SMOLITH")
  if [ -z "${ws_version:-}" ]; then
    ws_version=$(grep -m1 '^version *= *"0\.' "$SMOLITH" | grep -o '"[^"]*"' | tr -d '"' || true)
  fi
  if [ "${ws_version:-}" != "$EXPECTED" ]; then
    echo "check-smolith-pin: ../smolith workspace version is '${ws_version:-unknown}', expected $EXPECTED" >&2
    exit 1
  fi
  echo "check-smolith-pin: smolith checkout at $EXPECTED, rev $rev"
else
  echo "check-smolith-pin: no ../smolith checkout, skipping workspace check (rev $rev)"
fi

echo "check-smolith-pin: all laputa-* requirements at $EXPECTED"
