#!/usr/bin/env bash
# Run the ignored HTTP-only soak test for a caller-supplied duration.
#
# Usage: scripts/soak-test.sh [seconds]
#   seconds defaults to 86400 (24 hours). The duration is exported as
#   ONEGET_SOAK_SECONDS and the exact command is printed before it runs.
set -euo pipefail

duration=${1:-86400}
case "$duration" in
  ''|*[!0-9]*)
    printf 'usage: %s [seconds]\n' "$(basename "$0")" >&2
    exit 2
    ;;
esac

script_dir=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -P "$script_dir/.." && pwd -P)

export ONEGET_SOAK_SECONDS="$duration"
command_text="ONEGET_SOAK_SECONDS=$ONEGET_SOAK_SECONDS cargo test --test soak -- --ignored --nocapture"

printf 'soak duration: %s seconds\n' "$ONEGET_SOAK_SECONDS"
printf 'soak command: (cd %s && %s)\n' "$repo_root/src-tauri" "$command_text"

cd "$repo_root/src-tauri"
exec env ONEGET_SOAK_SECONDS="$ONEGET_SOAK_SECONDS" cargo test --test soak -- --ignored --nocapture
