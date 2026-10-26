#!/bin/sh
# Exercises guest/etc/orbit/orbit-runc's build-step path without Docker: a fake
# runc plays the build step against the bind sources. Runs as root inside the
# guest; devflow.sh pipes it in through nsenter. Usage: orbit-runc-test.sh <wrapper>
set -u
W=$1
T=/tmp/orbit-runc-test
pass=0 fail=0
ok() { pass=$((pass + 1)); echo "PASS $1"; }
no() { fail=$((fail + 1)); echo "FAIL $1"; }
pem() { printf -- '-----BEGIN CERTIFICATE-----\n%s\n-----END CERTIFICATE-----\n' "$1"; }
cleanup() { umount "$T/b/rootfs/etc/ssl" 2>/dev/null; rm -rf "$T"; }
setup() { # fresh bundle: image bundle has IMAGE, host CA is HOSTROOT
  cleanup
  mkdir -p "$T/b/rootfs/etc/ssl/certs" "$T/victim/ssl/certs"
  pem IMAGE > "$T/b/rootfs/etc/ssl/certs/ca-certificates.crt"
  pem VICTIM > "$T/victim/ssl/certs/ca-certificates.crt"
  pem HOSTROOT > "$T/host-ca.pem"
  echo '{"root":{"path":"rootfs"},"mounts":[{"destination":"/proc"}],"process":{"env":[]}}' > "$T/b/config.json"
  cat > "$T/runc" <<'R'
#!/bin/sh
# The "build step": acts on the directory bound at /etc/ssl/certs.
pem() { printf -- '-----BEGIN CERTIFICATE-----\n%s\n-----END CERTIFICATE-----\n' "$1"; }
src=$(jq -r '.mounts[] | select(.destination == "/etc/ssl/certs") | .source' "$B/config.json")
eval "$STEP"
R
  chmod +x "$T/runc"
}
run() { # <step script>
  B=$T/b STEP=$1 ORBIT_RUNC_BIN=$T/runc ORBIT_HOST_CA_FILE=$T/host-ca.pem \
    sh "$W" --root /run/x run --bundle "$T/b" --keep test-id >/dev/null 2>&1
}
has() { grep -q "$2" "$1" 2>/dev/null; }

setup
run 'pem ADDED >> "$src/ca-certificates.crt"'; rc=$?
f=$T/b/rootfs/etc/ssl/certs/ca-certificates.crt
if [ $rc = 0 ] && has "$f" IMAGE && has "$f" ADDED && ! has "$f" HOSTROOT; then ok "appended cert kept, host root removed"; else no "append (rc=$rc)"; fi

setup
run 'cp "$src/ca-certificates.crt" "$src/copy.pem"'; rc=$?
f=$T/b/rootfs/etc/ssl/certs/copy.pem
if [ $rc = 0 ] && has "$f" IMAGE && ! has "$f" HOSTROOT; then ok "copied bundle has no host root"; else no "copy (rc=$rc)"; fi

setup
mv "$T/b/rootfs/etc/ssl" "$T/b/rootfs/etc/ssl.real"; ln -s "$T/victim/ssl" "$T/b/rootfs/etc/ssl"
run 'touch "$src/x"'; rc=$?
if ! grep -q orbit-cadir "$T/b/config.json" && has "$T/victim/ssl/certs/ca-certificates.crt" VICTIM && [ ! -e "$T/victim/ssl/certs/x" ]; then ok "symlinked parent in the image: nothing bound, guest path untouched"; else no "symlinked parent"; fi

setup
run 'touch "$src/x"; rm -rf "$B/rootfs/etc/ssl"; ln -s "$T/victim/ssl" "$B/rootfs/etc/ssl"'; rc=$?
if [ $rc != 0 ] && has "$T/victim/ssl/certs/ca-certificates.crt" VICTIM && [ ! -e "$T/victim/ssl/certs/x" ]; then ok "parent swapped for a symlink during the step: refused, step fails"; else no "swapped parent (rc=$rc)"; fi

setup
mount -t tmpfs -o size=64k tmpfs "$T/b/rootfs/etc/ssl" && mkdir -p "$T/b/rootfs/etc/ssl/certs" && pem IMAGE > "$T/b/rootfs/etc/ssl/certs/ca-certificates.crt"
run 'dd if=/dev/zero of="$src/big" bs=1k count=48 2>/dev/null; dd if=/dev/zero of="$B/rootfs/etc/ssl/fill" bs=1k count=40 2>/dev/null'; rc=$?
if [ $rc != 0 ] && has "$T/b/rootfs/etc/ssl/certs/ca-certificates.crt" IMAGE && [ ! -e "$T/b/rootfs/etc/ssl/certs.orbit-new" ]; then ok "disk full during write-back: original kept, step fails"; else no "disk full (rc=$rc)"; fi

cleanup
echo "== orbit-runc: $pass pass, $fail fail"
[ $fail = 0 ]
