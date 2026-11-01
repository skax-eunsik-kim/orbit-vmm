#!/bin/bash
# orbit report: what someone needs to raise an issue, in two parts.
#   - A summary from a fixed list of fields, for the public issue. It holds no
#     log text and no setting values, so there is nothing in it to hide.
#   - A file with recent logs for the team, with personal details and secrets
#     hidden as far as a pattern can. It stays on the Mac until they send it.
# Nothing is sent: a draft opens in the browser and the person submits it.
# Installed as ~/.orbit/bin/report.sh. Runs only on what's on the Mac, so it
# works when the VM won't start.
set -euo pipefail
BIN="$(cd "$(dirname "$0")" && pwd)"
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
open_draft=1
[ "${1:-}" != --no-open ] || open_draft=0
export LC_ALL=C # logs can hold bytes that aren't valid text

version() { sed -n "s/^$1=//p" "$BIN/VERSION" 2>/dev/null || true; }
gib() { echo $(( $1 / 1073741824 )); }
vm_state() {
  local pid n
  pid=$(cat "$D/vmm.pid" 2>/dev/null || true)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    n=$(DOCKER_HOST="unix://$D/docker.sock" docker ps -q 2>/dev/null | wc -l | tr -d ' ') || n="?"
    echo "running, $n container(s)"
  else
    echo stopped
  fi
}
settings() {
  [ -f "$D/config.env" ] || return 0
  sed -n 's/^\(ORBIT_[A-Z0-9_]*\)=.*/\1/p' "$D/config.env" | paste -sd, - | sed 's/,/, /g'
}
crashes() { ls "$D/crashes" 2>/dev/null | wc -l | tr -d ' '; }

ORBIT_V=$(version version); ORBIT_V=${ORBIT_V:-unknown}
MAC="$(sysctl -n machdep.cpu.brand_string), $(gib "$(sysctl -n hw.memsize)") GB, macOS $(sw_vers -productVersion)"
SETTINGS=$(settings); SETTINGS=${SETTINGS:-none}
summary() {
  echo "orbit: $ORBIT_V"
  echo "Mac: $MAC"
  echo "Mac disk: $(df -k "$HOME" | awk 'NR == 2 { print int($4 / 1048576) }') GB free"
  echo "VM: $(vm_state)"
  echo "crashes recorded: $(crashes)"
  echo "settings changed: $SETTINGS"
}

escape() { printf '%s' "$1" | sed 's/[][\.*^$/|()+?{}]/\\&/g'; }
# Whole words only: a short name mustn't eat parts of other words.
word() { [ "${#1}" -lt 3 ] || printf 's|[[:<:]]%s[[:>:]]|<%s>|g\n' "$(escape "$1")" "$2"; }
hide_rules() {
  printf 's|%s|~|g\n' "$(escape "$HOME")"
  word "${USER:-}" user
  word "$(id -un)" user
  word "$(hostname -s)" host
  word "$(hostname)" host
  word "$(scutil --get LocalHostName 2>/dev/null || true)" host
  word "$(scutil --get ComputerName 2>/dev/null || true)" host
}
hide() {
  local rules
  rules=$(hide_rules)
  sed -E -e '/-----BEGIN /,/-----END /d' -e "$rules" \
    -e 's#(://)[^/@[:space:]]+@#\1<hidden>@#g' \
    -e 's/[Bb][Ee][Aa][Rr][Ee][Rr] +[^[:space:]]+/Bearer <hidden>/g' \
    -e 's/(([Tt][Oo][Kk][Ee][Nn]|[Ss][Ee][Cc][Rr][Ee][Tt]|[Pp][Aa][Ss][Ss][Ww]([Oo][Rr])?[Dd]|[Aa][Pp][Ii][_-]?[Kk][Ee][Yy]|[Cc][Rr][Ee][Dd][Ee][Nn][Tt][Ii][Aa][Ll])[[:alnum:]_-]*"?[[:space:]]*[:=][[:space:]]*)[^[:space:]]+/\1<hidden>/g' \
    -e 's/(ghp|gho|ghs|ghu|github_pat)_[[:alnum:]_]+/<hidden>/g' \
    -e 's/(sk|xoxb|xoxp)-[[:alnum:]_-]{10,}/<hidden>/g' \
    -e 's/AKIA[[:upper:][:digit:]]{12,}/<hidden>/g' \
    -e 's/eyJ[[:alnum:]_-]{10,}\.[[:alnum:]_.-]+/<hidden>/g' \
    -e 's/[[:alnum:]._%+-]+@[[:alnum:]-]+(\.[[:alnum:]-]+)*\.[[:alpha:]]{2,}/<email>/g'
}
section() { # section <title> <file> [lines]
  echo; echo "== $1"
  if [ -f "$2" ]; then tail -n "${3:-300}" "$2" | hide; else echo "(none)"; fi
}

umask 077
FILE="$HOME/orbit-report-$(date +%Y%m%d-%H%M%S).txt"
{
  summary
  section warnings "$D/warnings"
  section settings "$D/config.env"
  echo; echo "== crashes"; ls "$D/crashes" 2>/dev/null || echo "(none)"
  section watchdog.log "$D/watchdog.log" 200
  section console.log "$D/console.log" 300
} > "$FILE"

echo "For the issue:"
echo
summary | sed 's/^/  /'
echo
echo "Saved ~/${FILE##*/}, with recent logs for the team. Details are hidden by"
echo "pattern, so read it first, then send it privately. Don't attach it to the"
echo "issue: the repository is public."

encode() { # encode <text>: percent-encode for a URL
  local s=$1 i c
  for (( i = 0; i < ${#s}; i++ )); do
    c=${s:i:1}
    case $c in
      [a-zA-Z0-9.~_-]) printf '%s' "$c" ;;
      *) printf '%%%02X' "'$c" ;;
    esac
  done
}
REPO=$(version repo)
[ "$open_draft" = 1 ] && [ -n "$REPO" ] || exit 0
if [ "${ORBIT_REPORT_YES:-}" != 1 ]; then
  [ -t 0 ] || exit 0
  echo
  read -r -p "Open a draft issue with this summary? You submit it yourself. [y/N] " answer
  case $answer in y | Y | yes) ;; *) exit 0 ;; esac
fi
open "https://github.com/$REPO/issues/new?template=01-bug.yml&version=$(encode "$ORBIT_V")&mac=$(encode "$MAC")&env=$(encode "$SETTINGS")"
