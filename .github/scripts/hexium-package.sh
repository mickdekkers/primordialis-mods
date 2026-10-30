#!/usr/bin/env bash
# Makes the mod's package for Hexium, the mod site the Gale mod manager installs Primordialis mods from
# (the Thunderstore package format): a zip holding manifest.json, icon.png, README.md, CHANGELOG.md, the
# DLL and its PDB. Hexium holds native DLLs without a PDB for moderator review. The Release workflow
# runs it from the release commit, so that what goes with the DLL is from the same version, and
# releases the package with the DLL.
#
# The manifest is hexium/manifest.json with the version added. The README is the repository's, from the
# title down to the install steps (with its images linked on GitHub, at the tag), followed by
# hexium/mod-manager.md, which takes the install steps' place for players with a mod manager.
#
# Usage: hexium-package.sh <version> <primordialis_qol.dll> <primordialis_qol.pdb> <zip output>
set -euo pipefail

version=$1
dll=$2
pdb=$3
out=$4
repo=https://github.com/mickdekkers/primordialis-mods
tag="v$version"

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "::error::'$version' isn't a version like 1.2.3."
  exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

jq --arg version "$version" '. + {version_number: $version}' hexium/manifest.json > "$stage/manifest.json"
description=$(jq -r .description "$stage/manifest.json")
if (( ${#description} > 250 )); then
  echo "::error::The description in hexium/manifest.json is ${#description} characters long; Hexium takes 250."
  exit 1
fi

# Hexium only takes a 256×256 icon. A PNG's width and height are the big-endian numbers at bytes 16
# to 23.
size=$(od -An -tx1 -j16 -N8 assets/icon.png | tr -d ' \n')
if [[ $size != 0000010000000100 ]]; then
  echo "::error::assets/icon.png isn't 256×256."
  exit 1
fi
cp assets/icon.png "$stage/icon.png"

# The README up to its install steps, without the badges, whose links only work on GitHub.
awk '/^## Install$/ { exit } !/^\[!\[/ { print }' README.md |
  sed -E "s#\]\((docs/[^)]+)\)#]($repo/raw/$tag/\1)#g" |
  cat -s > "$stage/README.md"
if grep -qE '\]\((\.\./|docs/|#)' "$stage/README.md"; then
  echo "::error::The README has a relative link that wouldn't work on Hexium:"
  grep -nE '\]\((\.\./|docs/|#)' "$stage/README.md"
  exit 1
fi
sed -e "s#{{repo}}#$repo#g" -e "s#{{tag}}#$tag#g" hexium/mod-manager.md >> "$stage/README.md"

cp CHANGELOG.md "$stage/CHANGELOG.md"
cp "$dll" "$stage/primordialis_qol.dll"
cp "$pdb" "$stage/primordialis_qol.pdb"

rm -f "$out"
out=$(realpath "$out")
(cd "$stage" && zip -q -X "$out" manifest.json icon.png README.md CHANGELOG.md primordialis_qol.dll primordialis_qol.pdb)
unzip -l "$out"
