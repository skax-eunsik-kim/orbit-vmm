#!/bin/bash
# Runs guest/init-fssec against a fresh share; checks the host side afterwards.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"
BIN=${ORBIT_VMM_BIN:-$HERE/target/release/orbit-vmm}
S=$HERE/bench/fssec-share; O=$(mktemp -d "${TMPDIR:-/tmp}/orbit-outside.XXXXXX"); LOG="$O.log"
rm -rf "$S" "$O"; mkdir -p "$S/allowed/swap" "$O"
echo hello > "$S/allowed/file.txt"; echo nope > "$S/denied.txt"; echo in > "$S/allowed/swap/inside.txt"
echo TOP-SECRET > "$O/secret.txt"
# The VMM must refuse allowlist entries that aren't real paths or that expose
# the home directory, hidden directories or ~/Library.
ln -s "$S/allowed" "$S/link"
refused() { # refused <name> <allow path>
  if "$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd /dev/null --mem 256 --cpus 1 --share "/Users:users" \
       --share-allow "$2" >"$LOG.r" 2>&1 </dev/null; then echo "SEC FAIL host: accepted $1"
  elif grep -qE 'real path|refusing' "$LOG.r"; then echo "SEC ok   host: refuses $1"
  else echo "SEC FAIL host: $1 failed for another reason: $(tail -1 "$LOG.r")"; fi
}
refused "symlinked allow" "$S/link"
refused "case-variant allow" "$(echo "$S/allowed" | tr '[:lower:]' '[:upper:]')"
refused "home directory" "$HOME"
refused "hidden directory" "$HOME/.orbit"
mkdir -p "$S/allowed/inner"
refused2() { # like refused, with two allow paths
  if "$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd /dev/null --mem 256 --cpus 1 --share "/Users:users" \
       --share-allow "$2" --share-allow "$3" >"$LOG.r" 2>&1 </dev/null; then echo "SEC FAIL host: accepted $1"
  elif grep -q 'nested' "$LOG.r"; then echo "SEC ok   host: refuses $1"
  else echo "SEC FAIL host: $1 failed for another reason: $(tail -1 "$LOG.r")"; fi
}
refused2 "nested allows" "$S/allowed" "$S/allowed/inner"
rmdir "$S/allowed/inner"
rm -f "$S/link" "$LOG.r"
"$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd "$HOME/.orbit/vmm/fssec.cpio.gz" --mem 768 --cpus 2 \
  --share "$S:t" --share-allow "$S/allowed" > "$LOG" 2>&1 &
P=$!
for i in $(seq 1 200); do grep -q ready-for-swap "$LOG" && break; sleep 0.1; done
mv "$S/allowed/swap" "$S/allowed/swap.real" && ln -s "$O" "$S/allowed/swap" && touch "$S/allowed/swapped"
wait $P
grep '^SEC' "$LOG" | grep -v ready
[ -e "$O/planted.txt" ] && echo "SEC FAIL host: file planted outside the share" || echo "SEC ok   host: nothing written outside the share"
