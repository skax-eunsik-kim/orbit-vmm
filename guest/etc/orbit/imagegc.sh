#!/bin/sh
# Frees disk space when the Mac's disk or the data disk runs low. vmm-run.sh asks
# for this through control.sh ("gc <days>"); the data disk is a file on the Mac,
# so what Docker frees here is what the Mac gets back.
#
# Removes containers that stopped more than <days> ago, then images older than
# <days> that no container uses, whatever their tag, and build cache of the same
# age. Stacks are rebuilt or pulled again, so that's the price of a full disk.
# Then trims at once: memd only trims when the VM is quiet, and a VM filling a
# disk seldom is.
#
# Left alone: volumes, kind nodes (a stopped cluster should still start) and
# restart: always containers. Never `rmi -f`: an image that a container starts
# using meanwhile is refused.
#
# Usage: imagegc.sh <days> [reference]. A reference limits it to matching images
# and their containers and leaves the cache and the trim out, so devflow can't
# touch other stacks. IMAGEGC_STOPPED_BEFORE (YYYYMMDDhhmmss) is for that test
# too: a container can't be made to have stopped long ago.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
days=${1:-}; ref=${2:-}
case $days in '' | 0 | *[!0-9]*) exit 2 ;; esac
log() { echo "[imagegc] $*" > /dev/hvc0; }
exec 9> /run/orbit-imagegc.lock
flock -n 9 || exit 0

hours=$((days * 24))
# until= goes by when the tag first appeared here, which a rebuild doesn't move.
cutoff=$(date -u -d "@$(($(date +%s) - hours * 3600))" +%Y%m%d%H%M%S)
stopped_before=${IMAGEGC_STOPPED_BEFORE:-$cutoff}
digits() { echo "$1" | tr -dc 0-9 | cut -c1-14; }

stale=$(docker ps -a --no-trunc --filter status=exited --format '{{.ID}}') || exit 1
for id in $stale; do
  info=$(docker inspect -f '{{.State.FinishedAt}} {{.HostConfig.RestartPolicy.Name}} {{index .Config.Labels "io.x-k8s.kind.cluster"}}|{{.Config.Image}}|{{.Name}}' "$id") || continue
  image=${info#*|}; name=${image#*|}; image=${image%%|*}
  set -- ${info%%|*}
  [ "$(digits "$1")" -lt "$stopped_before" ] || continue
  [ "$2" != always ] && [ -z "${3:-}" ] || continue
  if [ -n "$ref" ]; then case $image in "$ref":*) ;; *) continue ;; esac; fi
  docker rm "$id" >/dev/null 2>&1 && log "removed stopped container ${name#/}"
done

# An empty list here would look like "nothing is in use".
ctrs=$(docker ps -aq --no-trunc) || exit 1
used=$(echo "$ctrs" | xargs -r docker inspect -f '{{.Image}}') || exit 1
list=$(docker images -a --no-trunc --filter "until=${hours}h" ${ref:+--filter "reference=$ref"} \
  --format '{{.ID}} {{.Repository}}:{{.Tag}} {{.CreatedAt}}') || exit 1
removed=0
while read -r id name created; do
  [ -n "$id" ] || continue
  [ "$(digits "$created")" -lt "$cutoff" ] || continue
  echo "$used" | grep -qxF "$id" && continue
  case $name in *'<none>'*) name=$id ;; esac
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
