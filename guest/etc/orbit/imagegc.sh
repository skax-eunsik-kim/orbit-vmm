#!/bin/sh
# Frees disk space when the Mac's disk runs low. vmm-run.sh asks for this through
# control.sh ("gc <days>"); the data disk is a file there, so what Docker frees
# here is what the Mac gets back.
#
# Removes images older than <days> (by build date) that no container uses, and
# build cache of the same age, then trims the disk at once: memd only trims when
# the VM is quiet, and a VM filling the Mac's disk seldom is.
#
# Left alone: latest, local and dev tags (often a stack's working images, say a
# sandbox image between runs) and volumes. Never `rmi -f`: an image that a
# container starts using meanwhile is refused.
#
# Usage: imagegc.sh <days> [reference]. A reference limits it to matching images
# and leaves the cache and the trim out, so devflow can't touch other stacks.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
days=${1:-}; ref=${2:-}
case $days in '' | 0 | *[!0-9]*) exit 2 ;; esac
log() { echo "[imagegc] $*" > /dev/hvc0; }
exec 9> /run/orbit-imagegc.lock
flock -n 9 || exit 0

hours=$((days * 24))
# An empty list here would look like "nothing is in use".
ctrs=$(docker ps -aq --no-trunc) || exit 1
used=$(echo "$ctrs" | xargs -r docker inspect -f '{{.Image}}') || exit 1
list=$(docker images -a --no-trunc --filter "until=${hours}h" ${ref:+--filter "reference=$ref"} \
  --format '{{.ID}} {{.Repository}}:{{.Tag}}') || exit 1

removed=0
while read -r id name; do
  [ -n "$id" ] || continue
  echo "$used" | grep -qxF "$id" && continue
  case $name in
    *:latest | *:local | *:dev) continue ;;
    *'<none>'*) name=$id ;;
  esac
  docker rmi "$name" >/dev/null 2>&1 || continue
  log "removed $name"
  removed=$((removed + 1))
done <<EOF
$list
EOF
log "removed $removed unused image(s) older than $days days"

[ -z "$ref" ] || exit 0
docker builder prune -f --filter "until=${hours}h" >/dev/null 2>&1
out=$(fstrim -v /data 2>&1) && log "fstrim: ${out}"
