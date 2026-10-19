#!/bin/sh
# Turns file changes made on the Mac into inotify events inside the guest, so
# watchers in containers (uvicorn --reload, vite, nodemon, ...) notice them.
# virtiofs can't forward host-side changes, so the VMM watches FSEvents and
# sends one line per change here (vsock port 1026).
#
# Protocol 3 (first line "orbit-fsnotify 3"), tab-separated, paths relative
# to the share:
#   M tag path         modified          -> IN_MODIFY (+ IN_CLOSE_WRITE)
#   C tag path         file created      -> IN_CREATE
#   D tag path         directory created -> IN_CREATE|IN_ISDIR
#   R tag path         file deleted      -> IN_DELETE
#   X tag path         directory deleted -> IN_DELETE|IN_ISDIR
#   V tag from to      renamed           -> IN_MOVED_FROM + IN_MOVED_TO
#   S                  let recursive watchers add the directories just created
# The change already happened on the Mac; we do the same thing here under
# NOTIFY_GID, and the VMM's virtio-fs server answers those requests without
# touching the Mac (it refuses anything under that gid that would change it,
# and only lets these through for paths it queued, while the Mac matches).
# So the guest kernel emits real inotify events for them.
# M uses a same-size truncate: also a no-op on the Mac, but IN_MODIFY here.
# Protocol 2 ("orbit-fsnotify 2", an older VMM) sends "tag<TAB>path" for
# modifications only; without either header (or without util-linux setpriv)
# fall back to open+close, which only yields IN_CLOSE_WRITE.
tab=$(printf '\t')
NOTIFY_GID=4000000001
IFS= read -r header
case "$header" in
  "orbit-fsnotify 3") proto=3 ;;
  "orbit-fsnotify 2") proto=2 ;;
  *) proto=1 ;;
esac
# util-linux setpriv (package "setpriv", replaces busybox's /bin/setpriv) is
# needed for --regid; without it stay on the open+close fallback.
setpriv --version 2>/dev/null | grep -q util-linux || proto=1
as_notify() { setpriv --regid=$NOTIFY_GID --clear-groups "$@" 2>/dev/null; }
mountpoint_of() { awk -v t="$1" '$1 == t && $3 == "virtiofs" {print $2; exit}' /proc/mounts; }
modify() { # modify <path>
  [ -f "$1" ] || return
  if [ $proto -ge 2 ]; then
    size=$(stat -c %s "$1" 2>/dev/null) || return
    as_notify truncate -c -s "$size" "$1"
  else
    # Never put the file name into socat's address string: socat splits it on
    # ',' and ':' and would take parts of a Mac file name as options. Open it
    # through a fixed-name symlink; without O_CREAT a vanished target just fails.
    ln -sfn "$1" /run/orbit-fsnotify-target && socat -u /dev/null OPEN:/run/orbit-fsnotify-target,wronly,append 2>/dev/null
  fi
}
handle() { # handle <op> <tag> <path> [<path2>]
  [ "$1" = S ] && { sleep 0.2; return; }
  [ "$2" = "$last_tag" ] || { last_tag=$2; mnt=$(mountpoint_of "$2"); }
  [ -n "$mnt" ] || return
  # Never stat the target itself first: that would cache it in the guest and
  # the create below could no longer happen.
  p="$mnt/$3"
  case "$1" in
    M) modify "$p" ;;
    # noclobber: O_CREAT|O_EXCL, never truncates. If the guest already has the
    # file cached (someone listed the directory), at least report a change.
    C) as_notify sh -Cc ': > "$1"' sh "$p" || modify "$p" ;;
    D) as_notify mkdir -- "$p" ;;
    R) as_notify rm -f -- "$p" || as_notify rmdir -- "$p" ;;
    X) as_notify rmdir -- "$p" || as_notify rm -f -- "$p" ;;
    V) [ -n "$4" ] && as_notify mv -f -T -- "$p" "$mnt/$4" ;;
  esac
}
if [ $proto = 3 ]; then
  while IFS="$tab" read -r op tag rel rel2; do handle "$op" "$tag" "$rel" "$rel2"; done
  exit 0
fi
[ $proto = 2 ] || { IFS="$tab" read -r tag rel <<EOF2
$header
EOF2
handle M "$tag" "$rel"; }
while IFS="$tab" read -r tag rel; do handle M "$tag" "$rel"; done
