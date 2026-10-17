#!/bin/bash
# Runs guest/init-fssec against a fresh share; checks the host side afterwards.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"
BIN=${ORBIT_VMM_BIN:-$HERE/target/release/orbit-vmm}
S=$HERE/bench/fssec-share; O=/tmp/orbit-outside
rm -rf "$S" "$O"; mkdir -p "$S/allowed/swap" "$O"
echo hello > "$S/allowed/file.txt"; echo nope > "$S/denied.txt"; echo in > "$S/allowed/swap/inside.txt"
echo TOP-SECRET > "$O/secret.txt"
"$BIN" --kernel "$HOME/.orbit/guest/kernel" --initrd "$HOME/.orbit/vmm/fssec.cpio.gz" --mem 768 --cpus 2 \
  --share "$S:t" --share-allow "$S/allowed" > /tmp/fssec.log 2>&1 &
P=$!
for i in $(seq 1 200); do grep -q ready-for-swap /tmp/fssec.log && break; sleep 0.1; done
mv "$S/allowed/swap" "$S/allowed/swap.real" && ln -s "$O" "$S/allowed/swap" && touch "$S/allowed/swapped"
wait $P
grep '^SEC' /tmp/fssec.log | grep -v ready
[ -e "$O/planted.txt" ] && echo "SEC FAIL host: file planted outside the share" || echo "SEC ok   host: nothing written outside the share"
