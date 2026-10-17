#!/bin/sh
# Turns file changes made on the Mac into inotify events inside the guest, so
# watchers in containers (uvicorn --reload, vite, nodemon, ...) notice them.
# virtiofs can't forward host-side changes, so the VMM watches FSEvents and
# sends "<tag>\t<path relative to the share>" lines here (vsock port 1026).
# For each, open the file for writing *without* O_CREAT and close it: content
# and mtime stay untouched, but the guest kernel emits IN_OPEN/IN_CLOSE_WRITE.
# (Deletions can't be replayed this way.)
tab=$(printf '\t')
# Protocol v2 (first line "orbit-fsnotify 2"): the VMM's virtio-fs server
# ignores SETATTR from NOTIFY_GID, so a same-size truncate under that gid is a
# pure no-op on the Mac yet makes the guest kernel emit IN_MODIFY, the event
# uvicorn/watchfiles and vite/chokidar react to. Without v2 (an older VMM would
# really truncate) fall back to open+close, which only yields IN_CLOSE_WRITE.
NOTIFY_GID=4000000001
IFS= read -r header
case "$header" in "orbit-fsnotify 2") v2=1 ;; *) v2=0 ;; esac
# util-linux setpriv (package "setpriv", replaces busybox's /bin/setpriv) is
# needed for --regid; without it stay on the open+close fallback.
setpriv --version 2>/dev/null | grep -q util-linux || v2=0
handle() {
  tag=$1 rel=$2
  mnt=$(awk -v t="$tag" '$1 == t && $3 == "virtiofs" {print $2; exit}' /proc/mounts)
  [ -n "$mnt" ] || return
  p="$mnt/$rel"
  [ -f "$p" ] || return
  if [ $v2 = 1 ]; then
    size=$(stat -c %s "$p" 2>/dev/null) || return
    setpriv --regid=$NOTIFY_GID --clear-groups truncate -c -s "$size" "$p" 2>/dev/null
  else
    # Never put the file name into socat's address string: socat splits it on
    # ',' and ':' and would take parts of a Mac file name as options. Open it
    # through a fixed-name symlink; without O_CREAT a vanished target just fails.
    ln -sfn "$p" /run/orbit-fsnotify-target && socat -u /dev/null OPEN:/run/orbit-fsnotify-target,wronly,append 2>/dev/null
  fi
}
[ $v2 = 1 ] || { IFS="$tab" read -r tag rel <<EOF2
$header
EOF2
handle "$tag" "$rel"; }
while IFS="$tab" read -r tag rel; do handle "$tag" "$rel"; done
