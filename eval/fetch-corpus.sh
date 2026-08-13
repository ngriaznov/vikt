#!/usr/bin/env bash
# Fetch the evaluation corpus: five production Python codebases, shallow.
#
# These are the corpora behind eval/RESULTS-corpus-scale.md and
# eval/RESULTS-real-code.md. Shallow clones because only the working tree is
# analyzed; nothing here needs history.
#
#   ./eval/fetch-corpus.sh [target-dir]      # default: ./corpus
set -euo pipefail
dest="${1:-corpus}"
mkdir -p "$dest"
for repo in psf/requests urllib3/urllib3 django/django \
            sqlalchemy/sqlalchemy pallets/flask Textualize/rich; do
    name="${repo#*/}"
    if [ -d "$dest/$name" ]; then
        echo "have    $name"
    else
        echo "cloning $name"
        git clone -q --depth 1 "https://github.com/$repo.git" "$dest/$name"
    fi
done
echo "corpus ready under $dest/"
