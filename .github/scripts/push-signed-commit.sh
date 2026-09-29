#!/usr/bin/env bash
# Puts a local commit on a branch of this GitHub repository as a commit GitHub signs. A pushed commit
# keeps its (missing) signature, but GitHub signs the commits a GitHub App creates through its API, as
# long as they have no custom author or committer. So the commit's changed files are uploaded, and the
# commit is recreated there: same tree, parent and message, with the App as its author. The tree is
# checked against the local one before anything is created, and the branch only fast-forwards: if it
# moved on from the commit's parent, it isn't changed.
#
# Needs GH_TOKEN (a GitHub App installation token that can write contents) and GITHUB_REPOSITORY. In
# GitHub Actions, sets the step output `commit` to the signed commit. With DRY_RUN set, it stops after
# checking the tree, before creating the commit.
#
# Usage: push-signed-commit.sh <commit> <branch>
set -euo pipefail

commit=$1
branch=$2
repo=${GITHUB_REPOSITORY:?}
parent=$(git rev-parse "$commit^")
tree=$(git rev-parse "$commit^{tree}")

# The changed files as tree entries, on top of the parent's tree. Each file is uploaded as a blob, which
# must hash to the same object it is locally.
entries=$(mktemp)
git diff-tree -r --no-renames "$parent" "$commit" | while IFS=$'\t' read -r meta path; do
  read -r old_mode mode _ sha status <<<"$meta"
  if [[ $status == D ]]; then
    jq -n --arg path "$path" --arg mode "${old_mode#:}" \
      '{path: $path, mode: $mode, type: "blob", sha: null}' >> "$entries"
  else
    uploaded=$(git cat-file blob "$sha" | base64 -w0 | jq -Rs '{content: ., encoding: "base64"}' |
      gh api "repos/$repo/git/blobs" --input - --jq .sha)
    if [[ $uploaded != "$sha" ]]; then
      echo "::error::Uploading $path gave blob $uploaded, not $sha."
      exit 1
    fi
    jq -n --arg path "$path" --arg mode "$mode" --arg sha "$sha" \
      '{path: $path, mode: $mode, type: "blob", sha: $sha}' >> "$entries"
  fi
done

uploaded_tree=$(jq -n --arg base "$(git rev-parse "$parent^{tree}")" --slurpfile entries "$entries" \
  '{base_tree: $base, tree: $entries}' | gh api "repos/$repo/git/trees" --input - --jq .sha)
if [[ $uploaded_tree != "$tree" ]]; then
  echo "::error::The uploaded tree is $uploaded_tree, not the commit's tree $tree."
  exit 1
fi
echo "Uploaded the tree of $commit ($tree)"
if [[ -n ${DRY_RUN:-} ]]; then
  exit 0
fi

created=$(git log -1 --format=%B "$commit" |
  jq -Rs --arg tree "$tree" --arg parent "$parent" '{message: ., tree: $tree, parents: [$parent]}' |
  gh api "repos/$repo/git/commits" --input -)
signed=$(jq -r .sha <<<"$created")
if [[ $(jq -r .verification.verified <<<"$created") != true ]]; then
  echo "::error::GitHub didn't sign the commit $signed ($(jq -r .verification.reason <<<"$created"))."
  exit 1
fi
echo "Created the signed commit $signed"

# Without force, GitHub only moves the branch if the commit builds on where it is now.
gh api --method PATCH "repos/$repo/git/refs/heads/$branch" -f sha="$signed" -F force=false > /dev/null
echo "Moved $branch to $signed"
echo "commit=$signed" >> "${GITHUB_OUTPUT:-/dev/null}"
