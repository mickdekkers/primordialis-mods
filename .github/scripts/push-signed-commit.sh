#!/usr/bin/env bash
# Puts a local commit on a branch of this GitHub repository as a commit GitHub signs. A GitHub App
# can't sign a commit it pushes, but GitHub signs the commits it creates itself, so the commit is made
# with GitHub's createCommitOnBranch API instead: the same changed files and message, on the same
# parent. The API only moves the branch if it's still at that parent (a fast-forward). The new commit
# is then checked to have the local commit's tree and a valid signature.
#
# Needs GH_TOKEN (a GitHub App installation token that can write contents) and GITHUB_REPOSITORY. In
# GitHub Actions, sets the step output `commit` to the signed commit.
#
# Usage: push-signed-commit.sh <commit> <branch>
set -euo pipefail

commit=$1
branch=$2
parent=$(git rev-parse "$commit^")
tree=$(git rev-parse "$commit^{tree}")

# The changed files: the path and contents of each added or modified one, the path of each deleted one.
changes=$(mktemp)
git diff-tree -r --no-renames --name-status "$parent" "$commit" | while IFS=$'\t' read -r status path; do
  if [[ $status == D ]]; then
    jq -n --arg path "$path" '{path: $path}'
  else
    git cat-file blob "$commit:$path" | base64 -w0 | jq -Rs --arg path "$path" '{path: $path, contents: .}'
  fi
done > "$changes"

# shellcheck disable=SC2016 # $input is a GraphQL variable
query='mutation($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) { commit { oid tree { oid } signature { isValid } } }
}'
created=$(jq -n --arg query "$query" --arg repo "${GITHUB_REPOSITORY:?}" --arg branch "$branch" \
  --arg parent "$parent" --arg headline "$(git log -1 --format=%s "$commit")" \
  --arg body "$(git log -1 --format=%b "$commit")" --slurpfile changes "$changes" '{
    query: $query,
    variables: {input: {
      branch: {repositoryNameWithOwner: $repo, branchName: $branch},
      expectedHeadOid: $parent,
      message: {headline: $headline, body: $body},
      fileChanges: {
        additions: [$changes[] | select(has("contents"))],
        deletions: [$changes[] | select(has("contents") | not)]
      }
    }}
  }' | gh api graphql --input - --jq .data.createCommitOnBranch.commit)
signed=$(jq -r .oid <<<"$created")
echo "Created $signed on $branch"

# The branch has already moved, so a failed check fails the release before it's published.
if [[ $(jq -r .tree.oid <<<"$created") != "$tree" ]]; then
  echo "::error::$signed doesn't have the tree of $commit ($tree)."
  exit 1
fi
if [[ $(jq -r .signature.isValid <<<"$created") != true ]]; then
  echo "::error::GitHub didn't sign $signed."
  exit 1
fi
echo "commit=$signed" >> "${GITHUB_OUTPUT:-/dev/null}"
