#!/usr/bin/env bash
# Asks Hexium whether a team's package has a version. Exits with 0 if it does, 1 if it doesn't, and 2
# if Hexium couldn't say.
#
# Usage: hexium-has.sh <team> <package> <version>
set -uo pipefail

url="https://primordialis.hexium.gg/api/experimental/package/$1/$2/$3/"
status=$(curl -sS -o /dev/null -w '%{http_code}' "$url")
case $status in
  200) exit 0 ;;
  404) exit 1 ;;
  *)
    echo "::error::Asking Hexium for $1-$2 $3 failed with HTTP $status."
    exit 2
    ;;
esac
