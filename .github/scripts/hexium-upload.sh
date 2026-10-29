#!/usr/bin/env bash
# Uploads a package made by hexium-package.sh to Hexium, as a new version of the team's package, in
# the Primordialis community's "Client-only" and "Open Source" categories. The same steps as the Gale
# mod manager's modpack upload, which uses Hexium's Thunderstore-compatible API: start an upload, send
# its parts, finish it, then submit it. Needs HEXIUM_API_TOKEN, an API token of the team.
#
# A version can only be uploaded once. Hexium scans it on VirusTotal before it shows.
#
# Usage: hexium-upload.sh <team> <zip>
set -euo pipefail

team=$1
zip=$2
# The community's own host: submitting through hexium.gg doesn't find the dependencies on it.
api=https://primordialis.hexium.gg/api/experimental
community=primordialis

if [[ -z ${HEXIUM_API_TOKEN:-} ]]; then
  echo "::error::No Hexium API token: the HEXIUM_API_TOKEN secret isn't set."
  exit 1
fi

# POSTs JSON to the API (a path under $api), printing the response. Fails on an error status, after
# showing its body.
post() {
  local response status
  response=$(mktemp)
  status=$(curl -sS -o "$response" -w '%{http_code}' -X POST "$api/$1" \
    -H "Authorization: Bearer $HEXIUM_API_TOKEN" -H 'Content-Type: application/json' --data "$2")
  if [[ $status != 2?? ]]; then
    echo "::error::POST $1 failed with HTTP $status: $(head -c 2000 "$response")" >&2
    rm -f "$response"
    return 1
  fi
  cat "$response"
  rm -f "$response"
}

size=$(stat -c %s "$zip")
start=$(post usermedia/initiate-upload/ \
  "$(jq -n --arg name "$(basename "$zip")" --argjson size "$size" '{filename: $name, file_size_bytes: $size}')")
uuid=$(jq -er .user_media.uuid <<<"$start")
echo "Uploading $(basename "$zip") ($size bytes) as $uuid, in $(jq '.upload_urls | length' <<<"$start") parts."

# Each part goes to the URL it was given. Its ETag says it arrived, for finishing the upload.
parts='[]'
part=$(mktemp)
headers=$(mktemp)
trap 'rm -f "$part" "$headers"' EXIT
while read -r number url offset length; do
  tail -c +"$((offset + 1))" "$zip" | head -c "$length" > "$part"
  if ! curl -sSf -X PUT "$url" --data-binary "@$part" -D "$headers" -o /dev/null; then
    echo "::error::Uploading part $number failed."
    post "usermedia/$uuid/abort-upload/" '{}' > /dev/null || true
    exit 1
  fi
  etag=$(tr -d '\r' < "$headers" | sed -n 's/^[Ee][Tt][Aa][Gg]: *//p' | tail -n1)
  if [[ -z $etag ]]; then
    echo "::error::Part $number got no ETag."
    post "usermedia/$uuid/abort-upload/" '{}' > /dev/null || true
    exit 1
  fi
  parts=$(jq --arg etag "$etag" --argjson number "$number" '. + [{ETag: $etag, PartNumber: $number}]' <<<"$parts")
done < <(jq -r '.upload_urls[] | "\(.part_number) \(.url) \(.offset) \(.length)"' <<<"$start")

post "usermedia/$uuid/finish-upload/" "$(jq -n --argjson parts "$parts" '{parts: $parts}')" > /dev/null

submission=$(jq -n --arg team "$team" --arg uuid "$uuid" --arg community "$community" '{
  author_name: $team,
  upload_uuid: $uuid,
  communities: [$community],
  categories: [],
  community_categories: {($community): ["Client-only", "Open Source"]},
  has_nsfw_content: false
}')
result=$(post submission/submit/ "$submission")
full_name=$(jq -er .package_version.full_name <<<"$result")
name=$(jq -er .package_version.name <<<"$result")
echo "Submitted $full_name."

if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  echo "Uploaded [$full_name](https://$community.hexium.gg/mods/$team/$name) to Hexium." >> "$GITHUB_STEP_SUMMARY"
fi
