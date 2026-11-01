#!/usr/bin/env bash
set -euo pipefail

cpu="$(cat /sys/fs/cgroup/cpu.max)"
memory="$(cat /sys/fs/cgroup/memory.max)"
swap="$(cat /sys/fs/cgroup/memory.swap.max)"

printf 'cpu.max %s\nmemory.max %s\nmemory.swap.max %s\n' "$cpu" "$memory" "$swap"

fail=0
if [ "$cpu" != "100000 100000" ]; then
  echo "cpu.max is '$cpu', expected '100000 100000'" >&2
  fail=1
fi
if [ "$memory" != "2147483648" ]; then
  echo "memory.max is '$memory', expected '2147483648'" >&2
  fail=1
fi
if [ "$swap" != "0" ]; then
  echo "memory.swap.max is '$swap', expected '0'" >&2
  fail=1
fi
exit "$fail"
