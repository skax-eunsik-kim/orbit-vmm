#!/bin/bash
# Finds out about new releases, for everyone: the supervisor runs this while
# the VM is up. It never installs anything: orbit update does.
#   update-check.sh         look up the latest release, at most once a day, and
#                           notify once per release newer than the installed one
#   update-check.sh --hint  print a line if a newer one is known (no network)
# Remembers latest= and notified= in update-state; its time is the last lookup.
set -u
BIN="$(cd "$(dirname "$0")" && pwd)"
STATE="$BIN/update-state"
DAY=86400

field() { sed -n "s/^$2=//p" "$1" 2>/dev/null | head -1; }
# A number that sorts like the release; `git describe` adds -3-gabc1234 to it.
rank() {
  [[ ${1:-} =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+) ]] || { echo 0; return; }
  echo $(( BASH_REMATCH[1] * 1000000 + BASH_REMATCH[2] * 1000 + BASH_REMATCH[3] ))
}
newer() { [ "$(rank "$1")" -gt "$(rank "$installed")" ]; }

installed=$(field "$BIN/VERSION" version)
latest=$(field "$STATE" latest)

if [ "${1:-}" = --hint ]; then
  ! newer "$latest" || echo "$latest is available (installed: $installed). Run orbit update."
  exit 0
fi

if [ -f "$STATE" ] && [ $(( $(date +%s) - $(stat -f %m "$STATE") )) -lt "$DAY" ]; then exit 0; fi
repo=$(field "$BIN/VERSION" repo)
[ -n "$repo" ] || exit 0
# Not the REST API: unauthenticated it allows 60 calls an hour per IP, which a
# company's shared egress address uses up (seen: 403, 0 remaining). The web page
# redirects to the latest release's tag with no such limit. /usr/bin/curl uses
# the macOS trust store, so it works behind TLS inspection.
url=$("${ORBIT_CURL:-/usr/bin/curl}" -sS -o /dev/null --max-time 10 -w '%{redirect_url}' \
  "${ORBIT_RELEASES_URL:-https://github.com/$repo/releases/latest}" 2>/dev/null)
tag=${url##*/tag/}
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 0

notified=$(field "$STATE" notified)
if newer "$tag" && [ "$notified" != "$tag" ]; then
  notified=$tag
  if [ "${ORBIT_NOTIFY:-1}" != 0 ]; then
    # Text goes in as arguments, never into the AppleScript source.
    osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title (item 2 of argv)' -e 'end run' \
      "$tag is available (installed: $installed). Run orbit update." orbit-vmm >/dev/null 2>&1 || true
  fi
fi
printf 'latest=%s\nnotified=%s\n' "$tag" "$notified" > "$STATE"
