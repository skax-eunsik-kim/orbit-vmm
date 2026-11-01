#!/bin/sh
# What imagegc.sh removes and keeps. Runs in the guest (see devflow.sh); $1 is the
# script. It's limited to orbittest/gc tags, so other images are safe.
GC=$1
fail=0
# A file keeps the images apart (tags of one image would share its use) and gives
# them a build date, which a label alone doesn't.
image() { # image <tag> [build flag...]
  t=$1; shift
  printf 'FROM scratch\nCOPY <<EOF /gc\n%s\nEOF\n' "$t" |
    docker build -q --no-cache "$@" -t "orbittest/gc:$t" - >/dev/null
}
old() { image "$1" --build-arg SOURCE_DATE_EPOCH=1000000000; }
has() { docker image inspect "orbittest/gc:$1" >/dev/null 2>&1; }
gone() { ! has "$1"; }
volume() { docker volume inspect orbittest-gc >/dev/null 2>&1; }
want() { "$@" || { echo "not as expected: $*"; fail=1; }; }
clean() {
  docker rm -f orbittest-gc >/dev/null 2>&1
  docker volume rm orbittest-gc >/dev/null 2>&1
  for t in unused stopped stopped2 latest local dev new rebuilt arrived arrived-src; do docker rmi "orbittest/gc:$t" >/dev/null 2>&1; done
}
clean
for t in unused stopped latest local dev; do old "$t"; done
image new
old rebuilt
image rebuilt
# Old by build date but new here, like an image pulled today.
old arrived-src
docker tag orbittest/gc:arrived-src orbittest/gc:arrived
docker rmi orbittest/gc:arrived-src >/dev/null
docker tag orbittest/gc:stopped orbittest/gc:stopped2
docker create --name orbittest-gc orbittest/gc:stopped true >/dev/null
docker volume create orbittest-gc >/dev/null

"$GC" 7 orbittest/gc || { echo "imagegc exited $?"; fail=1; }

want gone unused
for t in stopped stopped2 latest local dev new rebuilt arrived; do want has "$t"; done
want volume
"$GC" 0 >/dev/null 2>&1; want test $? = 2
"$GC" '7;x' >/dev/null 2>&1; want test $? = 2
clean
exit $fail
