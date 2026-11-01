#!/usr/bin/env bash
set -euo pipefail

action="${1:-check}"

nginx_running() {
  local pid state
  while read -r pid; do
    state="$(ps -o stat= -p "$pid" 2>/dev/null || true)"
    case "$state" in
      ""|Z*) ;;
      *) return 0 ;;
    esac
  done < <(pgrep -x nginx 2>/dev/null || true)
  return 1
}

start_nginx() {
  nginx -t
  if ! nginx_running; then
    nginx
  fi
  for _ in $(seq 50); do
    if curl -s -o /dev/null http://127.0.0.1/; then
      return 0
    fi
    sleep 0.1
  done
  echo "nginx did not answer on port 80" >&2
  return 1
}

stop_nginx() {
  if nginx_running; then
    nginx -s quit >/dev/null 2>&1 || true
  fi
  for _ in $(seq 50); do
    if ! nginx_running; then
      return 0
    fi
    sleep 0.1
  done
  if nginx_running; then
    nginx -s stop >/dev/null 2>&1 || true
  fi
  for _ in $(seq 50); do
    if ! nginx_running; then
      return 0
    fi
    sleep 0.1
  done
  echo "nginx did not stop" >&2
  return 1
}

case "$action" in
  start)
    start_nginx
    echo "nginx listening on port 80"
    ;;
  stop)
    stop_nginx
    ;;
  check)
    start_nginx
    code="$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1/)"
    if [ "$code" != 502 ]; then
      echo "expected 502 from nginx with no app running, got $code" >&2
      exit 1
    fi
    echo "nginx port 80 -> $code (no app running)"
    ;;
  *)
    echo "usage: nginx-check.sh [start|stop|check]" >&2
    exit 2
    ;;
esac
