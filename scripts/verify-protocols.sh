#!/usr/bin/env sh
# Protocol fork gate. Runs each fork suite, prints its test counts, and fails on
# a red suite or a suite that ran no tests.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

mkdir -p "$root/.work/tmp"
TMPDIR="$root/.work/tmp"
CARGO_TARGET_DIR="$root/.work/tmp/protocol-target"
export TMPDIR CARGO_TARGET_DIR

H2_MANIFEST="crates/laputa-h2-futures-lite/Cargo.toml"
HYPER_MANIFEST="crates/laputa-hyper-futures-lite/Cargo.toml"
WS_MANIFEST="crates/laputa-fastwebsockets-futures-lite/Cargo.toml"
SUPPORT_MANIFEST="crates/laputa-protocol-test-support/Cargo.toml"
LEDGER="docs/fork-test-parity.md"

selected_suites=""
check_ledger=0

usage() {
    cat <<'EOF'
usage: sh scripts/verify-protocols.sh [--suite NAME]... [--check-ledger]

Suites: h2-lib h2-flow h2-inherited hyper-check hyper-tests fastwebsockets support
With no --suite, every suite runs. --check-ledger only validates the ledger.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --suite)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            selected_suites="$selected_suites $2"
            shift 2
            ;;
        --suite=*)
            selected_suites="$selected_suites ${1#--suite=}"
            shift
            ;;
        --check-ledger)
            check_ledger=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

is_selected() {
    [ -z "$selected_suites" ] && return 0
    for name in $selected_suites; do
        [ "$name" = "$1" ] && return 0
    done
    return 1
}

counts_file=$(mktemp "$TMPDIR/protocol-counts.XXXXXX")
trap 'rm -f "$counts_file"' EXIT HUP INT TERM

record_counts() {
    printf '%s|%s|%s|%s\n' "$1" "$2" "$3" "$4" >>"$counts_file"
}

check_counts() {
    suite=$1
    passed=$2
    failed=$3
    ignored=$4

    if [ "$passed" -eq 0 ]; then
        printf 'FAIL: %s ran zero tests\n' "$suite" >&2
        return 1
    fi
    if [ "$failed" -ne 0 ]; then
        printf 'FAIL: %s reported %s failed tests\n' "$suite" "$failed" >&2
        return 1
    fi

    printf 'PASS: %-15s passed=%s failed=%s ignored=%s\n' \
        "$suite" "$passed" "$failed" "$ignored"
    record_counts "$suite" "$passed" "$failed" "$ignored"
}

run_tests() {
    suite=$1
    shift
    log="$TMPDIR/protocol-$suite.log"

    if ! "$@" >"$log" 2>&1; then
        cat "$log" >&2
        printf 'FAIL: %s command failed: %s\n' "$suite" "$*" >&2
        return 1
    fi
    cat "$log"

    summary_count=$(awk '/^test result: / { count++ } END { print count + 0 }' "$log")
    if [ "$summary_count" -eq 0 ]; then
        printf 'FAIL: %s printed no test result summary\n' "$suite" >&2
        return 1
    fi

    set -- $(awk '/^test result: / {
        for (i = 1; i <= NF; i++) {
            if ($i == "passed;") passed += $(i - 1)
            if ($i == "failed;") failed += $(i - 1)
            if ($i == "ignored;") ignored += $(i - 1)
        }
    }
    END { print passed + 0, failed + 0, ignored + 0 }' "$log")
    passed=$1
    failed=$2
    ignored=$3

    case "$suite" in
        h2-inherited|hyper-tests|fastwebsockets|support)
            check_counts "$suite" "$passed" "$failed" "$ignored"
            ;;
        *)
            printf 'PASS: %-15s passed=%s failed=%s ignored=%s\n' \
                "$suite" "$passed" "$failed" "$ignored"
            record_counts "$suite" "$passed" "$failed" "$ignored"
            ;;
    esac
}

run_check() {
    suite=$1
    shift
    log="$TMPDIR/protocol-$suite.log"

    if ! "$@" >"$log" 2>&1; then
        cat "$log" >&2
        printf 'FAIL: %s command failed: %s\n' "$suite" "$*" >&2
        return 1
    fi
    printf 'PASS: %-15s check ok\n' "$suite"
}

check_ledger_file() {
    if [ ! -f "$LEDGER" ]; then
        printf 'FAIL: ledger %s is missing\n' "$LEDGER" >&2
        return 1
    fi

    pending=$(awk -F'|' '
        /^\|/ &&
        $2 ~ /^[[:space:]]*(h2|hyper|fastwebsockets|shared)[[:space:]]*$/ &&
        $8 ~ /not ported/ { count++ }
        END { print count + 0 }
    ' "$LEDGER")
    if [ "$pending" -ne 0 ]; then
        printf 'FAIL: ledger has %s row(s) still marked not ported\n' "$pending" >&2
        return 1
    fi

    exclusions=$(awk -F'|' '
        /^\|/ && $2 ~ /^[[:space:]]*(h2|hyper|fastwebsockets|shared)[[:space:]]*$/ {
            value = $7
            gsub(/[^0-9]/, "", value)
            if (value != "") total += value
        }
        END { print total + 0 }
    ' "$LEDGER")
    for fork in h2 hyper; do
        count=$(awk -F'|' -v fork="$fork" '
            /^\|/ && $2 !~ /Fork/ && $2 !~ /^[[:space:]]*-+[[:space:]]*$/ &&
            $2 ~ "^[[:space:]]*" fork "[[:space:]]*$" {
                value = $7
                gsub(/[^0-9]/, "", value)
                if (value != "") total += value
            }
            END { print total + 0 }
        ' "$LEDGER")
        if [ "$count" -gt 10 ]; then
            printf 'FAIL: %s ledger has %s exclusions, cap is 10\n' "$fork" "$count" >&2
            return 1
        fi
    done

    while IFS='|' read -r _ fork suite _ reference _ _ status command _; do
        fork=$(printf '%s' "$fork" | sed 's/^ *//; s/ *$//')
        status=$(printf '%s' "$status" | sed 's/^ *//; s/ *$//')
        case "$fork" in
            h2|hyper|fastwebsockets|shared) ;;
            *) continue ;;
        esac
        [ "$status" = "excluded" ] && continue
        command=$(printf '%s' "$command" | sed 's/^ *//; s/ *$//; s/^`//; s/`$//')
        [ -z "$command" ] && continue
        case "$command" in *wake_loss_is_detected*) continue ;; esac
        reference=$(printf '%s' "$reference" | tr -d ' ')
        case "$reference" in ''|*[!0-9]*) continue ;; esac

        list_output=$(mktemp "$TMPDIR/ledger-list.XXXXXX")
        if ! sh -c "$command -- --list" >"$list_output" 2>&1; then
            cat "$list_output" >&2
            rm -f "$list_output"
            printf 'FAIL: ledger suite %s could not list tests\n' "$suite" >&2
            return 1
        fi
        actual=$(grep -c ': test$' "$list_output" || true)
        rm -f "$list_output"
        if [ "$actual" -ne "$reference" ]; then
            printf 'FAIL: ledger suite %s says %s tests but --list found %s\n' \
                "$suite" "$reference" "$actual" >&2
            return 1
        fi
    done <"$LEDGER"

    printf 'PASS: ledger statuses and --list counts match (%s exclusions)\n' "$exclusions"
}

print_counts_summary() {
    printf '\nSuite counts:\n'
    while IFS='|' read -r suite passed failed ignored; do
        printf '  %-15s passed=%s failed=%s ignored=%s\n' \
            "$suite" "$passed" "$failed" "$ignored"
    done <"$counts_file"
}

if [ "$check_ledger" -eq 1 ]; then
    check_ledger_file
fi

if [ "$check_ledger" -eq 1 ] && [ -z "$selected_suites" ]; then
    exit 0
fi

start=$(date +%s)

if is_selected h2-lib; then
    run_tests h2-lib cargo test --manifest-path "$H2_MANIFEST" --lib
fi
if is_selected h2-flow; then
    run_tests h2-flow cargo test --manifest-path "$H2_MANIFEST" --test flow_control_futures
fi
if is_selected h2-inherited; then
    run_tests h2-inherited cargo test --manifest-path "$H2_MANIFEST" -p h2-tests
fi
if is_selected hyper-check; then
    run_check hyper-check cargo check --manifest-path "$HYPER_MANIFEST" --features full
fi
if is_selected hyper-tests; then
    run_tests hyper-tests cargo test --manifest-path "$HYPER_MANIFEST" --features full --tests
fi
if is_selected fastwebsockets; then
    run_tests fastwebsockets cargo test --manifest-path "$WS_MANIFEST" --locked --all-features
fi
if is_selected support; then
    if [ -f "$SUPPORT_MANIFEST" ]; then
        run_tests support cargo test --manifest-path "$SUPPORT_MANIFEST"
    else
        printf 'FAIL: support suite requested but %s is missing\n' "$SUPPORT_MANIFEST" >&2
        exit 1
    fi
fi

end=$(date +%s)
wall=$((end - start))
print_counts_summary
printf '\nwall=%ss\n' "$wall"
