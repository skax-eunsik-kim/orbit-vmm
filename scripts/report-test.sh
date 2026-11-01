#!/bin/bash
# What orbit report keeps and hides, on a made-up install. Needs no VM.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export HOME="$T/home" USER=reportuser
unset ORBIT_INSTANCE
HOST=$(hostname -s)
D="$HOME/.orbit/vmm"
mkdir -p "$HOME/.orbit/bin" "$D/crashes/20261101-101010"
cp "$ROOT/scripts/report.sh" "$HOME/.orbit/bin/"
printf 'version=v9.9.9\nrepo=example/orbit\n' > "$HOME/.orbit/bin/VERSION"
fail=0
want() { # want <what> <command...>
  local what=$1; shift
  "$@" || { echo "FAIL $what"; fail=1; }
}
hidden() { ! grep -qF -- "$2" "$1"; }
shown() { grep -qF -- "$2" "$1"; }
report() { "$HOME/.orbit/bin/report.sh" --no-open </dev/null 2>&1; }

# Nothing there yet: no logs, VM stopped.
out=$(report)
want "works with no logs" test $? = 0
want "says the VM is stopped" grep -q 'VM: stopped' <<< "$out"
rm -f "$HOME"/orbit-report-*.txt

cat > "$D/console.log" <<EOF
[imagegc] removed orbittest/gc:unused
pulled alpine@sha256:abc123 for $HOME/project
user $USER on host $HOST
TOKEN=plain-token-value password: hunter2 "api_key": "k-123"
Authorization: Bearer bearer-value
clone https://bob:url-pass@gitlab:8080/x.git
ghp_0123456789abcdefghijklmnopqrstuvwxyz and sk-abcdefghijklmnop1234
mail me at someone@example.com
-----BEGIN CERTIFICATE-----
Y2VydGlmaWNhdGU=
-----END CERTIFICATE-----
EOF
echo "[2026-11-01 10:00:00] Mac disk has 8 GB free at $HOME" > "$D/watchdog.log"
echo "disk warning" > "$D/warnings"
printf 'ORBIT_MEM_MIB=4096\nORBIT_SHARE_ALLOW=%s/work\n' "$HOME" > "$D/config.env"

out=$(report)
f=$(echo "$HOME"/orbit-report-*.txt)
want "writes the file" test -f "$f"
want "file is the owner's alone" test "$(stat -f %Lp "$f")" = 600
want "keeps ordinary lines" shown "$f" '[imagegc] removed orbittest/gc:unused'
want "keeps image digests" shown "$f" 'alpine@sha256:abc123'
want "keeps watchdog log" shown "$f" 'Mac disk has 8 GB free'
want "keeps setting names" shown "$f" 'ORBIT_MEM_MIB=4096'
want "lists crashes" shown "$f" '20261101-101010'
for s in "$HOME" "$USER" "$HOST" plain-token-value hunter2 k-123 bearer-value url-pass \
  ghp_0123456789 sk-abcdefghijklmnop someone@example.com Y2VydGlmaWNhdGU; do
  want "hides $s" hidden "$f" "$s"
done

# The issue draft holds fixed fields only.
sum=$T/summary
echo "$out" > "$sum"
want "summary has the version" shown "$sum" 'orbit: v9.9.9'
want "summary has setting names" shown "$sum" 'ORBIT_SHARE_ALLOW'
for s in "/work" hunter2 "Mac disk has" imagegc; do
  want "summary leaves out $s" hidden "$sum" "$s"
done

# It only opens a browser when asked: a stand-in `open` would record it.
mkdir "$T/shim"
printf '#!/bin/sh\necho "$1" > "%s/opened"\n' "$T" > "$T/shim/open"
chmod +x "$T/shim/open"
PATH="$T/shim:$PATH" "$HOME/.orbit/bin/report.sh" </dev/null >/dev/null 2>&1
want "no terminal, no browser" test ! -e "$T/opened"
PATH="$T/shim:$PATH" ORBIT_REPORT_YES=1 "$HOME/.orbit/bin/report.sh" </dev/null >/dev/null 2>&1
want "opens a draft when told to" test -e "$T/opened"
want "draft goes to the repository's bug form" grep -q '^https://github.com/example/orbit/issues/new?template=01-bug.yml&' "$T/opened"
want "draft is encoded" grep -q 'version=v9.9.9' "$T/opened"
for s in "/work" "$HOST" "$USER"; do want "draft leaves out $s" hidden "$T/opened" "$s"; done

[ "$fail" = 0 ] && echo "orbit report: pass"
exit "$fail"
