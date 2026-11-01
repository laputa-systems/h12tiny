#!/usr/bin/env bash
set -euo pipefail

CHALLENGE_REV=d9de6942dcc3669bac9459603d32516dc230359d
CHALLENGE_REMOTE=https://github.com/arjaythedev/twelve-dollar-server-challenge.git

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
CHALLENGE="$ROOT/.work/twelve-dollar/challenge"

if [ ! -e "$CHALLENGE" ]; then
  mkdir -p "$(dirname "$CHALLENGE")"
  git clone --quiet "$CHALLENGE_REMOTE" "$CHALLENGE"
elif [ ! -d "$CHALLENGE/.git" ]; then
  echo "refusing to use $CHALLENGE: not a git checkout" >&2
  exit 1
fi

head="$(git -C "$CHALLENGE" rev-parse HEAD)"
if [ "$head" != "$CHALLENGE_REV" ]; then
  echo "challenge checkout is $head, expected $CHALLENGE_REV" >&2
  exit 1
fi

bash "$CHALLENGE/seed/make-seed.sh"

dirty="$(git -C "$CHALLENGE" status --porcelain)"
if [ -n "$dirty" ]; then
  echo "challenge checkout is not clean after seeding:" >&2
  printf '%s\n' "$dirty" >&2
  exit 1
fi
