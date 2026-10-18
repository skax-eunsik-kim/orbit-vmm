#!/bin/sh
# Hands memory back to the Mac. Free page reporting (virtio-balloon) returns
# *free* guest pages, but Linux keeps freed file data as page cache and leaves
# free memory fragmented, so after builds or with a stack running the VM stays
# much bigger on the Mac than what the guest actually uses.
#
#  - quiet (CPU >= 85% idle, light disk I/O) for QUIET_S: reclaim half of the
#    containers' page cache (cgroup memory.reclaim, gentle on running services)
#    and compact, so freed pages form blocks that get reported;
#  - idle (CPU >= 95% idle) for IDLE_S: drop all clean page cache and the
#    dentry/inode caches (a build leaves ~1 GB of them; on virtio-fs dropping
#    them also lets the host forget its nodes), compact;
#  - always: keep page cache under CACHE_MB. A build or image pull is never
#    idle, and without a cap it grew the cache to 7 GB (14 GB on the Mac)
#    until it finished. The cache is redundant anyway: the Mac caches data.img
#    itself. Reclaim goes through the root cgroup's memory.reclaim, which
#    takes the coldest pages first.
#
# Tunables (kernel command line): orbit.memd_idle=<s> (30), orbit.memd_quiet=<s>
# (60), orbit.memd_min_mb=<MiB> (128: leave caches smaller than this alone),
# orbit.memd_cache_mb=<MiB> (1024; 0 turns the cap off), orbit.memd_trim=<s>
# (600: fstrim /data this often when quiet; 0 turns it off).
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
IDLE_S=30; QUIET_S=60; MIN_MB=128; CACHE_MB=1024; TRIM_S=600
for arg in $(cat /proc/cmdline); do
  case "$arg" in
    orbit.memd_idle=*) IDLE_S=${arg#orbit.memd_idle=} ;;
    orbit.memd_quiet=*) QUIET_S=${arg#orbit.memd_quiet=} ;;
    orbit.memd_min_mb=*) MIN_MB=${arg#orbit.memd_min_mb=} ;;
    orbit.memd_cache_mb=*) CACHE_MB=${arg#orbit.memd_cache_mb=} ;;
    orbit.memd_trim=*) TRIM_S=${arg#orbit.memd_trim=} ;;
  esac
done
case "$CACHE_MB" in '' | *[!0-9]*) CACHE_MB=1024 ;; esac
case "$TRIM_S" in '' | *[!0-9]*) TRIM_S=600 ;; esac
INTERVAL=5
# Defragment in the background too, so free memory stays reportable.
sysctl -qw vm.compaction_proactiveness=50 2>/dev/null

cpu() { awk '/^cpu /{print $2+$3+$4+$5+$6+$7+$8+$9, $5+$6}' /proc/stat; }
io() { awk '/^(pgpgin|pgpgout) /{s+=$2} END{print s}' /proc/vmstat; }
# Reclaimable page cache: tmpfs shows up as Cached too but can't be dropped.
cached_mb() { awk '/^Cached:/{c=$2} /^Shmem:/{s=$2} END{print int((c-s)/1024)}' /proc/meminfo; }
log() { echo "[memd] $*" > /dev/hvc0; }

set -- $(cpu); last_total=$1; last_idle=$2
last_io=$(io)
idle_for=0; quiet_for=0; trimmed=0
while sleep $INTERVAL; do
  set -- $(cpu); total=$1; idle=$2
  now_io=$(io)
  dt=$((total - last_total)); di=$((idle - last_idle)); dio=$((now_io - last_io))
  last_total=$total; last_idle=$idle; last_io=$now_io
  [ $dt -gt 0 ] || continue
  pct=$((di * 100 / dt))
  if [ $pct -ge 85 ] && [ $dio -lt $((4096 * INTERVAL)) ]; then quiet_for=$((quiet_for + INTERVAL)); else quiet_for=0; fi
  if [ $pct -ge 95 ] && [ $dio -lt $((1024 * INTERVAL)) ]; then idle_for=$((idle_for + INTERVAL)); else idle_for=0; trimmed=0; fi

  # Cap with 25% slack so a cache hovering at the limit isn't reclaimed every tick.
  if [ "$CACHE_MB" -gt 0 ]; then
    c=$(cached_mb)
    if [ "$c" -gt $((CACHE_MB + CACHE_MB / 4)) ]; then
      echo "$((c - CACHE_MB))M" > /sys/fs/cgroup/memory.reclaim 2>/dev/null
      capped=$((${capped:-0} + c - CACHE_MB))
    fi
    # One summary line a minute at most, not one per tick.
    tick=$((${tick:-0} + 1))
    if [ $((tick % 12)) -eq 0 ] && [ "${capped:-0}" -gt 0 ]; then
      log "cap ${CACHE_MB} MiB: reclaimed ~${capped} MiB of page cache in the last minute (now $(cached_mb) MiB)"
      capped=0
    fi
  fi

  # Disk: blocks Docker freed (pruned images, build cache) still take space in
  # data.img until they are discarded; fstrim punches them out of the file.
  # Once soon after boot (space freed by earlier versions), then every TRIM_S
  # when the VM is quiet.
  since_trim=$((${since_trim:-$TRIM_S} + INTERVAL))
  if [ "$TRIM_S" -gt 0 ] && [ $since_trim -ge "$TRIM_S" ] && [ $quiet_for -ge 30 ]; then
    since_trim=0
    out=$(fstrim -v /data 2>&1) && log "fstrim: ${out}"
  fi

  if [ $idle_for -ge $IDLE_S ] && [ $trimmed -eq 0 ]; then
    before=$(cached_mb)
    trimmed=1
    slab=$(awk '/^SReclaimable:/{print int($2/1024)}' /proc/meminfo)
    [ "$before" -ge "$MIN_MB" ] || [ "$slab" -ge "$MIN_MB" ] || continue
    slab_before=$(awk '/^SReclaimable:/{print int($2/1024)}' /proc/meminfo)
    sync
    echo 3 > /proc/sys/vm/drop_caches
    echo 1 > /proc/sys/vm/compact_memory
    log "idle ${idle_for}s: page cache ${before} -> $(cached_mb) MiB, reclaimable slab ${slab_before} -> $(awk '/^SReclaimable:/{print int($2/1024)}' /proc/meminfo) MiB"
  elif [ $quiet_for -ge $QUIET_S ]; then
    quiet_for=0
    cg=/sys/fs/cgroup/docker
    [ -w $cg/memory.reclaim ] || continue
    file_mb=$(awk '/^file /{print int($2/1048576)}' $cg/memory.stat)
    [ "${file_mb:-0}" -ge "$MIN_MB" ] || continue
    echo "$((file_mb / 2))M" > $cg/memory.reclaim 2>/dev/null
    echo 1 > /proc/sys/vm/compact_memory
    log "quiet: containers' page cache ${file_mb} -> $(awk '/^file /{print int($2/1048576)}' $cg/memory.stat) MiB"
  fi
done
