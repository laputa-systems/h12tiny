#!/usr/bin/env bash
set -euo pipefail

out=""
pid=""
port=""
db=""
pidfile=""
interval=1

while [ $# -gt 0 ]; do
  case "$1" in
    --out)
      out="${2:?--out needs a path}"
      shift
      ;;
    --out=*)
      out="${1#*=}"
      ;;
    --pid)
      pid="${2:?--pid needs a process id}"
      shift
      ;;
    --pid=*)
      pid="${1#*=}"
      ;;
    --port)
      port="${2:?--port needs a port}"
      shift
      ;;
    --port=*)
      port="${1#*=}"
      ;;
    --db)
      db="${2:?--db needs a path}"
      shift
      ;;
    --db=*)
      db="${1#*=}"
      ;;
    --pidfile)
      pidfile="${2:?--pidfile needs a path}"
      shift
      ;;
    --pidfile=*)
      pidfile="${1#*=}"
      ;;
    --interval)
      interval="${2:?--interval needs seconds}"
      shift
      ;;
    --interval=*)
      interval="${1#*=}"
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

[ -n "$out" ] || { echo "--out is required" >&2; exit 2; }
[ -n "$pid" ] || { echo "--pid is required" >&2; exit 2; }
[ -n "$port" ] || { echo "--port is required" >&2; exit 2; }
[ -n "$db" ] || { echo "--db is required" >&2; exit 2; }

mkdir -p "$(dirname "$out")"
if [ -n "$pidfile" ]; then
  mkdir -p "$(dirname "$pidfile")"
  printf '%s\n' "$$" > "$pidfile"
fi

clk_tck="$(getconf CLK_TCK)"
start_ms="$(date +%s%3N)"
sample_index=0

header="sample,timestamp_ms,elapsed_s,clk_tck,app_pid,app_cpu_ticks,nginx_pids,nginx_cpu_ticks,app_rss_kib,app_hwm_kib,app_fds,established,db_bytes,wal_bytes,memory_current_bytes,memory_peak_bytes,cpu_usage_usec,cpu_user_usec,cpu_system_usec,cpu_nr_periods,cpu_nr_throttled,cpu_throttled_usec,pids_current"
printf '%s\n' "$header" > "$out"

read_ticks() {
  local target="$1" stat rest
  [ -r "/proc/$target/stat" ] || return 0
  stat="$(cat "/proc/$target/stat" 2>/dev/null || true)"
  [ -n "$stat" ] || return 0
  rest="${stat##*) }"
  set -- $rest
  printf '%s' "$(( ${12:-0} + ${13:-0} ))"
}

nginx_ticks() {
  local total=0 any=0 p ticks
  for p in $(pgrep -x nginx 2>/dev/null || true); do
    ticks="$(read_ticks "$p")"
    if [ -n "$ticks" ]; then
      total=$((total + ticks))
      any=1
    fi
  done
  printf '%s' "$total"
  [ "$any" -eq 1 ] || true
}

nginx_pid_list() {
  pgrep -x nginx 2>/dev/null | paste -sd+ - || true
}

status_field() {
  local name="$1" value
  value="$(awk -v want="$name:" '$1 == want { print $2; exit }' "/proc/$pid/status" 2>/dev/null || true)"
  printf '%s' "$value"
}

fd_count() {
  local count
  count="$(ls "/proc/$pid/fd" 2>/dev/null | wc -l)"
  printf '%s' "${count:-0}"
}

file_bytes() {
  local size
  size="$(stat -c %s "$1" 2>/dev/null || true)"
  printf '%s' "${size:-0}"
}

cgroup_value() {
  cat "$1" 2>/dev/null || true
}

cpu_stat_field() {
  local key="$1" stat="$2"
  printf '%s' "$stat" | awk -v want="$key" '$1 == want { print $2; exit }'
}

sample() {
  local now_ms elapsed app_ticks rss hwm fds established db_bytes wal_bytes
  local mem_current mem_peak pids_current cpu_stat usage user system periods throttled throttled_usec

  now_ms="$(date +%s%3N)"
  elapsed="$(awk -v now="$now_ms" -v start="$start_ms" 'BEGIN { printf "%.3f", (now - start) / 1000 }')"
  app_ticks="$(read_ticks "$pid")"
  rss="$(status_field VmRSS)"
  hwm="$(status_field VmHWM)"
  fds="$(fd_count)"
  established="$(ss -H -tn state established "sport = :$port" 2>/dev/null | wc -l)"
  db_bytes="$(file_bytes "$db")"
  wal_bytes="$(file_bytes "$db-wal")"
  mem_current="$(cgroup_value /sys/fs/cgroup/memory.current)"
  mem_peak="$(cgroup_value /sys/fs/cgroup/memory.peak)"
  pids_current="$(cgroup_value /sys/fs/cgroup/pids.current)"
  cpu_stat="$(cgroup_value /sys/fs/cgroup/cpu.stat)"
  usage="$(cpu_stat_field usage_usec "$cpu_stat")"
  user="$(cpu_stat_field user_usec "$cpu_stat")"
  system="$(cpu_stat_field system_usec "$cpu_stat")"
  periods="$(cpu_stat_field nr_periods "$cpu_stat")"
  throttled="$(cpu_stat_field nr_throttled "$cpu_stat")"
  throttled_usec="$(cpu_stat_field throttled_usec "$cpu_stat")"

  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$sample_index" "$now_ms" "$elapsed" "$clk_tck" "$pid" "$app_ticks" \
    "$(nginx_pid_list)" "$(nginx_ticks)" "$rss" "$hwm" "$fds" "$established" \
    "$db_bytes" "$wal_bytes" "$mem_current" "$mem_peak" \
    "$usage" "$user" "$system" "$periods" "$throttled" "$throttled_usec" "$pids_current" \
    >> "$out"
  sample_index=$((sample_index + 1))
}

finish() {
  sample || true
  exit 0
}

trap finish TERM INT

while :; do
  sample || true
  sleep "$interval"
done
