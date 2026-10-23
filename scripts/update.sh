#!/bin/bash
# Updates orbit to the latest release, or goes back to the previous one.
#
#   make update              latest release tag
#   make update TO=v0.2.1    a given tag
#   make rollback            back to what was installed before the last update
#   NOW=1                    restart the VM even if containers are running
#
# It builds in its own clone, ~/.orbit/src, not in this checkout: the checkout
# is often in a directory containers can write to. Before replacing anything it
# keeps the installed binaries and guest as ~/.orbit/bin.prev and
# ~/.orbit/guest.prev; rollback swaps them back (so a second rollback redoes
# the update). Restarting the VM stops its containers, so it restarts only
# when none are running, or with NOW=1.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOME_DIR="${ORBIT_HOME:-$HOME/.orbit}"
SRC="${ORBIT_SRC:-$HOME_DIR/src}"
BIN="${ORBIT_BIN:-$HOME_DIR/bin}"
GUEST="$HOME_DIR/guest"
SOCK="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}/docker.sock"
URL="${ORBIT_UPDATE_URL:-$(git -C "$ROOT" remote get-url origin)}"
mode=update now=0 to=""
while [ $# -gt 0 ]; do
  case "$1" in
    --rollback) mode=rollback ;;
    --now) now=1 ;;
    --to) to=$2; shift ;;
    *) echo "usage: $0 [--to TAG] [--now] | --rollback [--now]" >&2; exit 2 ;;
  esac
  shift
done
die() { echo "$*" >&2; exit 1; }
installed() { sed -n 's/^version=//p' "$BIN/VERSION" 2>/dev/null; }

restart() {
  local pid
  pid=$(cat "$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}/vmm.pid" 2>/dev/null || true)
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || { echo "the VM isn't running; start it with make up"; return; }
  local n
  n=$(DOCKER_HOST="unix://$SOCK" docker ps -q 2>/dev/null | wc -l | tr -d ' ')
  if [ "$now" = 1 ] || [ "$n" = 0 ]; then
    "$BIN/vmm-up.sh"
  else
    echo "$n container(s) running, so the VM keeps the old version until you restart it: make up (or NOW=1)"
  fi
}

# Swap two directories (rename is atomic per directory; the VM keeps running
# from files it already opened).
swap() {
  mv "$1" "$1.swap"
  mv "$2" "$1"
  mv "$1.swap" "$2"
}

if [ "$mode" = rollback ]; then
  [ -d "$BIN.prev" ] && [ -d "$GUEST.prev" ] || die "nothing to roll back to ($BIN.prev / $GUEST.prev missing)"
  from=$(installed)
  swap "$BIN" "$BIN.prev"
  swap "$GUEST" "$GUEST.prev"
  echo "rolled back ${from:-?} -> $(installed)"
  restart
  exit 0
fi

[ -d "$SRC/.git" ] || git clone -q "$URL" "$SRC"
git -C "$SRC" fetch -q --tags --force origin
tag=${to:-$(git -C "$SRC" tag -l 'v[0-9]*' --sort=-v:refname | head -n 1)}
[ -n "$tag" ] || die "no release tags in $URL yet"
git -C "$SRC" rev-parse -q --verify "refs/tags/$tag" >/dev/null || die "no tag $tag in $URL"
[ "$(installed)" != "$tag" ] || { echo "already at $tag"; exit 0; }

echo "updating $(installed || true) -> $tag (building in $SRC)"
git -C "$SRC" -c advice.detachedHead=false checkout -q --detach "$tag"
make -C "$SRC/vmm" build >/dev/null
# Build the new guest in a staging directory first (sharing the download
# cache): if a download or the build fails, nothing installed has changed.
stage="$HOME_DIR/.update-stage"
rm -rf "$stage"; mkdir -p "$stage" "$HOME_DIR/cache"; ln -s "$HOME_DIR/cache" "$stage/cache"
trap 'rm -rf "$stage"' EXIT
ORBIT_HOME="$stage" "$SRC/scripts/fetch-guest.sh" >/dev/null
rm -rf "$BIN.prev" "$GUEST.prev"
[ -d "$BIN" ] && cp -Rp "$BIN" "$BIN.prev"
ORBIT_BIN="$BIN" "$SRC/scripts/install.sh"
# Older releases' install.sh doesn't write VERSION; this run knows the tag.
repo=$(sed -n 's/^repo=//p' "$BIN.prev/VERSION" 2>/dev/null || true)
[ -n "$repo" ] || repo=$(echo "$URL" | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')
printf 'version=%s\nrepo=%s\n' "$tag" "$repo" > "$BIN/VERSION"
[ -d "$GUEST" ] && mv "$GUEST" "$GUEST.prev"
mv "$stage/guest" "$GUEST"
echo "installed $tag (make rollback goes back)"
restart
