#!/bin/bash
# When update-check.sh tells people about a release. Needs no VM or network.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export HOME="$T/home" PATH="$T/shim:$PATH" ORBIT_CURL="$T/curl"
unset ORBIT_INSTANCE ORBIT_NOTIFY
BIN=$T/bin
mkdir -p "$HOME/.orbit" "$BIN" "$T/shim"
cp "$ROOT/scripts/update-check.sh" "$ROOT/scripts/orbit" "$BIN/"
printf '#!/bin/sh\necho "$7" >> "%s/notes"\n' "$T" > "$T/shim/osascript"
# Stands in for GitHub: counts lookups, answers with the redirect in $T/answer.
printf '#!/bin/sh\necho x >> "%s/lookups"\ncat "%s/answer"\n' "$T" "$T" > "$T/curl"
chmod +x "$T/shim/osascript" "$T/curl"
fail=0
want() { # want <what> <command...>
  local what=$1; shift
  "$@" || { echo "FAIL $what"; fail=1; }
}
installed() { printf 'version=%s\nrepo=example/orbit\n' "$1" > "$BIN/VERSION"; }
released() { echo "https://github.com/example/orbit/releases/tag/$1" > "$T/answer"; }
check() { "$BIN/update-check.sh"; }
count() { [ -f "$1" ] && wc -l < "$1" | tr -d ' ' || echo 0; }
notes() { count "$T/notes"; }
lookups() { count "$T/lookups"; }
next_day() { [ ! -f "$BIN/update-state" ] || touch -t 202001010000 "$BIN/update-state"; }
hint() { "$BIN/orbit" status 2>/dev/null | grep -c 'orbit update'; }
is() { [ "$2" = "$3" ] || { echo "  $1: got '$2', want '$3'"; return 1; }; }

installed v0.2.5; released v0.2.5; check
want "up to date: no notice" is notes "$(notes)" 0
want "up to date: no hint" is hint "$(hint)" 0

next_day; released v0.2.6; check
want "newer release: one notice" is notes "$(notes)" 1
want "notice names the release" grep -q 'v0.2.6 is available (installed: v0.2.5)' "$T/notes"
want "status shows the hint" is hint "$(hint)" 1
want "orbit up's hint" grep -q 'v0.2.6' <<< "$("$BIN/update-check.sh" --hint)"

n=$(lookups); check
want "asks once a day" is lookups "$(lookups)" "$n"
next_day; check
want "asks again the next day" is lookups "$(lookups)" "$((n + 1))"
want "same release: no second notice" is notes "$(notes)" 1

next_day; released v0.2.10; check
want "next release: another notice (10 > 6)" is notes "$(notes)" 2

installed v0.2.10
want "updated: hint gone" is hint "$(hint)" 0

installed v0.2.10-3-gabc1234; next_day; check
want "a build past the release isn't behind it" is notes "$(notes)" 2

for bad in '' 'https://github.com/example/orbit/releases' '.../tag/v9.9.9;x'; do
  echo "$bad" > "$T/answer"; next_day; check
done
want "unexpected answers: no notice" is notes "$(notes)" 2
next_day; n=$(lookups)
printf '#!/bin/sh\necho x >> "%s/lookups"\nexit 7\n' "$T" > "$T/curl"
check; want "a failed lookup exits cleanly" test $? = 0
check
want "a failed lookup is tried again" is lookups "$(lookups)" "$((n + 2))"

printf '#!/bin/sh\necho x >> "%s/lookups"\ncat "%s/answer"\n' "$T" "$T" > "$T/curl"
next_day; released v0.3.0; ORBIT_NOTIFY=0 "$BIN/update-check.sh"
want "ORBIT_NOTIFY=0: no notice" is notes "$(notes)" 2
want "ORBIT_NOTIFY=0: the hint stays" is hint "$(hint)" 1

[ "$fail" = 0 ] && echo "update check: pass"
exit "$fail"
