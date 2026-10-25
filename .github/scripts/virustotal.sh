#!/usr/bin/env bash
# Uploads a file to VirusTotal, waits for the scan to finish, and writes the result as Markdown for
# the release notes: a badge with the number of engines that flagged the file, linking to the full
# report. With --rescan, VirusTotal scans the file it already has again instead (by its SHA-256), with
# the engines' current signatures. Needs VT_API_KEY. In GitHub Actions, sets the step output `clean` to
# `true` when the scan finished and no engine flagged the file, and `badge` to the badge alone, for the
# README.
#
# The Markdown is wrapped in the comments `<!-- virustotal -->` and `<!-- /virustotal -->`, which don't
# show on GitHub, so that a rescan can replace it in the release notes.
#
# Usage: virustotal.sh [--rescan] <file> <markdown output>
set -euo pipefail

rescan=false
if [[ $1 == --rescan ]]; then
  rescan=true
  shift
fi
file=$1
out=$2
today=$(date -u +%Y-%m-%d)
name=$(basename "$file")
sha256=$(sha256sum "$file" | cut -d' ' -f1)
report="https://www.virustotal.com/gui/file/$sha256"
api=https://www.virustotal.com/api/v3

if [[ -z ${VT_API_KEY:-} ]]; then
  echo "::error::No VirusTotal API key: the VIRUSTOTAL_API_KEY secret isn't set."
  exit 1
fi

set_clean() {
  echo "clean=$1" >> "${GITHUB_OUTPUT:-/dev/null}"
}

set_badge() {
  echo "badge=$1" >> "${GITHUB_OUTPUT:-/dev/null}"
}

# The free API allows 4 requests a minute, so failed requests (e.g. rate limited) are retried slowly.
vt() {
  curl --silent --show-error --fail-with-body --retry 5 --retry-all-errors --retry-delay 20 \
    --header "x-apikey: $VT_API_KEY" "$@"
}

if [[ $rescan == true ]]; then
  analysis=$(vt --request POST "$api/files/$sha256/analyse" | jq -r '.data.id // empty')
  if [[ -z $analysis ]]; then
    echo "::error::VirusTotal didn't start a rescan of $name ($sha256)."
    exit 1
  fi
  echo "Asked for a rescan of $name ($sha256), analysis $analysis"
else
  analysis=$(vt --form "file=@$file" "$api/files" | jq -r '.data.id // empty')
  if [[ -z $analysis ]]; then
    echo "::error::VirusTotal didn't accept $name."
    exit 1
  fi
  echo "Uploaded $name ($sha256), analysis $analysis"
fi

# Scans usually take a few minutes, but a new file can wait in VirusTotal's queue for longer than 20
# (v0.2.4's did). Give up after an hour, checking twice a minute: well within the public API's
# limits of 4 requests a minute and 500 a day.
status=
for _ in $(seq 120); do
  sleep 30
  result=$(vt "$api/analyses/$analysis") || continue
  status=$(jq -r '.data.attributes.status' <<<"$result")
  echo "Scan status: $status"
  [[ $status == completed ]] && break
done

if [[ $status != completed ]]; then
  echo "::warning::The VirusTotal scan of $name didn't finish in time."
  cat >"$out" <<EOF
<!-- virustotal -->
### VirusTotal

The scan of \`$name\` on $today didn't finish in time. The [report]($report) has the results.
<!-- /virustotal -->
EOF
  set_clean false
  set_badge "[![VirusTotal: scan not finished](https://img.shields.io/badge/VirusTotal-report-lightgrey)]($report)"
  exit 0
fi

# Engines that gave a verdict; those that timed out or can't scan this type of file don't count.
counts=$(jq -r '.data.attributes.stats
  | "\(.malicious + .suspicious) \(.malicious + .suspicious + .undetected + .harmless)"' <<<"$result")
read -r flagged total <<<"$counts"
# An unreadable result must never pass for a clean one.
if ! [[ $flagged =~ ^[0-9]+$ && $total =~ ^[1-9][0-9]*$ ]]; then
  echo "::error::Unexpected VirusTotal result: $result"
  exit 1
fi
echo "$flagged of $total engines flagged $name"

if ((flagged == 0)); then
  color=brightgreen
  # Only a clean scan is published, so this is the result on the day of the release.
  verdict="None of the $total antivirus engines on VirusTotal flagged \`$name\` when it was released ($today)."
  set_clean true
else
  if ((flagged <= 3)); then color=yellow; else color=red; fi
  verdict="$flagged of the $total antivirus engines on VirusTotal flagged \`$name\` on $today. The report shows which."
  echo "::warning::$flagged of $total VirusTotal engines flagged $name: $report"
  set_clean false
fi

badge="[![VirusTotal: $flagged/$total detections](https://img.shields.io/badge/VirusTotal-${flagged}%2F${total}%20detections-${color})]($report)"
set_badge "$badge"
cat >"$out" <<EOF
<!-- virustotal -->
### VirusTotal

$badge

$verdict [Full report]($report)
<!-- /virustotal -->
EOF
