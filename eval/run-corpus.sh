#!/usr/bin/env bash
# Run the batch harness over every fetched corpus plus the local stdlib, and
# print the one-line summary per corpus. Full logs land in eval/corpus-logs/.
#
#   ./eval/fetch-corpus.sh && ./eval/run-corpus.sh [corpus-dir]
#
# VIKT_SCORER=current re-measures the incumbent alone for comparison.
set -euo pipefail
dir="${1:-corpus}"
logs=eval/corpus-logs
mkdir -p "$logs"
cargo build --release --example evaluate -q

declare -A roots=(
    [requests]="$dir/requests/src/requests"
    [urllib3]="$dir/urllib3/src/urllib3"
    [django]="$dir/django/django"
    [sqlalchemy]="$dir/sqlalchemy/lib/sqlalchemy"
    [flask]="$dir/flask/src/flask"
    [rich]="$dir/rich/rich"
    [stdlib]="$(python3 -c 'import sysconfig; print(sysconfig.get_paths()["stdlib"])')"
)
for name in requests urllib3 django sqlalchemy flask rich stdlib; do
    root="${roots[$name]}"
    [ -d "$root" ] || { echo "skip $name (no $root)"; continue; }
    ./target/release/examples/evaluate py "$root" > "$logs/$name.log" 2>&1
    printf '%-11s' "$name"
    grep -E "^functions|^analysis|^wall" "$logs/$name.log" \
        | tr '\n' ' ' | sed 's/  */ /g'
    echo
done
echo "full logs: $logs/"
