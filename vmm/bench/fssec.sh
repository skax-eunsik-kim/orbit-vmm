#!/bin/bash
# Runs guest/init-fssec against a fresh share; checks the host side afterwards.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"
BIN=${ORBIT_VMM_BIN:-$HERE/target/release/orbit-vmm}
S=$HERE/bench/fssec-share; O=$(mktemp -d "${TMPDIR:-/tmp}/orbit-outside.XXXXXX"); LOG="$O.log"
rm -rf "$S" "$O"; mkdir -p "$S/allowed/swap" "$O"
echo hello > "$S/allowed/file.txt"; echo nope > "$S/denied.txt"; echo in > "$S/allowed/swap/inside.txt"
echo TOP-SECRET > "$O/secret.txt"
# The VMM must refuse allowlist entries that expose the home directory, hidden
# directories or ~/Library (whatever case or symlinks they are given with).
refused() { # refused <name> <allow path>
  if "$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd /dev/null --mem 256 --cpus 1 --share "/Users:users" \
       --share-allow "$2" >"$LOG.r" 2>&1 </dev/null; then echo "SEC FAIL host: accepted $1"
  elif grep -qE 'real path|refusing' "$LOG.r"; then echo "SEC ok   host: refuses $1"
  else echo "SEC FAIL host: $1 failed for another reason: $(tail -1 "$LOG.r")"; fi
}
refused "home directory in other case" "$(echo "$HOME" | tr '[:lower:]' '[:upper:]')"
refused "home directory" "$HOME"
refused "hidden directory" "$HOME/.orbit"
if "$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd /dev/null --mem 256 --cpus 1 --share "$HOME:h" >"$LOG.r" 2>&1 </dev/null; then
  echo "SEC FAIL host: accepted home as share with no allowlist"
elif grep -q refusing "$LOG.r"; then echo "SEC ok   host: refuses home as share with no allowlist"
else echo "SEC FAIL host: home share failed for another reason: $(tail -1 "$LOG.r")"; fi
mkdir -p "$S/allowed/inner"
refused2() { # refused2 <name> <expected error> <allow path> <allow path>
  if "$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd /dev/null --mem 256 --cpus 1 --share "/Users:users" \
       --share-allow "$3" --share-allow "$4" >"$LOG.r" 2>&1 </dev/null; then echo "SEC FAIL host: accepted $1"
  elif grep -q "$2" "$LOG.r"; then echo "SEC ok   host: refuses $1"
  else echo "SEC FAIL host: $1 failed for another reason: $(tail -1 "$LOG.r")"; fi
}
refused2 "nested allows" nested "$S/allowed" "$S/allowed/inner"
rmdir "$S/allowed/inner"
# A symlink inside a shared dir (which a container could repoint) must not
# decide what another entry shares.
mkdir -p "$S/other"; ln -s "$S/other" "$S/allowed/link"
refused2 "allow via a symlink in a share" "resolves through" "$S/allowed" "$S/allowed/link"
rm -f "$S/allowed/link"
# Same, with the shared directory stored as NFD on disk but given as NFC (how
# Hangul and accented names usually get typed).
NFD=$(python3 -I -c 'import unicodedata;print(unicodedata.normalize("NFD","작업"))')
NFC=$(python3 -I -c 'import unicodedata;print(unicodedata.normalize("NFC","작업"))')
mkdir -p "$S/$NFD"; ln -s "$S/other" "$S/$NFD/link"
refused2 "allow via a symlink in a share (NFC vs NFD)" "resolves through" "$S/$NFC" "$S/$NFC/link"
rm -f "$S/$NFD/link"; rmdir "$S/$NFD"
rm -f "$LOG.r"
"$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd "$HOME/.orbit/vmm/fssec.cpio.gz" --mem 768 --cpus 2 \
  --share "$S:t" --share-allow "$S/allowed" > "$LOG" 2>&1 &
P=$!
for i in $(seq 1 200); do grep -q ready-for-swap "$LOG" && break; sleep 0.1; done
mv "$S/allowed/swap" "$S/allowed/swap.real" && ln -s "$O" "$S/allowed/swap" && touch "$S/allowed/swapped"
wait $P
grep '^SEC' "$LOG" | grep -v ready
m=$(stat -f %Lp "$S/allowed/suid" 2>/dev/null)
[ "$m" = 755 ] && echo "SEC ok   host: guest chmod 4777 is 755 on the Mac" || echo "SEC FAIL host: guest chmod 4777 left mode $m on the Mac"
[ -e "$O/planted.txt" ] && echo "SEC FAIL host: file planted outside the share" || echo "SEC ok   host: nothing written outside the share"
