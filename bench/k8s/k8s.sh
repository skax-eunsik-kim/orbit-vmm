#!/bin/bash
# Kubernetes checks for `orbit k8s` (kind): create a cluster, run pods that
# pull, resolve, serve, store and reach HTTPS through the Mac's roots, then
# delete it. Also records the VM's memory on the Mac (phys_footprint) before,
# with the cluster idle, under 20 pods, and after deletion.
# Usage: k8s.sh [orbit command]   (default ~/.orbit/bin/orbit; ORBIT_INSTANCE as usual)
# Uses cluster orbittest-k8s and its own KUBECONFIG, so your kubectl config and
# other clusters are left alone.
set -u
ORBIT=${1:-$HOME/.orbit/bin/orbit}
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
export DOCKER_HOST="unix://$D/docker.sock" KUBECONFIG
KUBECONFIG=$(mktemp)
NAME=orbittest-k8s
OUT="$(dirname "$0")/results-k8s${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}.txt"
: > "$OUT"
pass=0 fail=0
now() { python3 -c 'import time;print(time.time())'; }
secs() { python3 -c "print('%.1f' % ($2-$1))"; }
report() { printf '%-5s %-44s %s\n' "$1" "$2" "$3" | tee -a "$OUT"; }
check() { # check <name> <command...>
  local name=$1; shift
  local t0 out
  t0=$(now)
  if out=$("$@" 2>&1); then pass=$((pass + 1)); report PASS "$name" "$(secs "$t0" "$(now)")s"
  else fail=$((fail + 1)); report FAIL "$name" "$(secs "$t0" "$(now)")s | $(echo "$out" | tr '\n' ' ' | cut -c1-160)"; fi
}
mib() { footprint -p "$(cat "$D/vmm.pid")" 2>/dev/null | awk '/phys_footprint:/{v=$2; if ($3 == "GB") v *= 1024; else if ($3 == "KB") v /= 1024; printf "%d", v; exit}'; }
k() { kubectl --context "kind-$NAME" --request-timeout=30s "$@"; }
run_pod() { # <name> <image> <command>: runs to completion, prints its log
  k run "$1" --image="$2" --restart=Never --command -- sh -c "$3" >/dev/null || return 1
  local phase="" i
  for i in $(seq 1 120); do
    phase=$(k get pod "$1" -o jsonpath='{.status.phase}' 2>/dev/null)
    case $phase in Succeeded | Failed) break ;; esac
    sleep 2
  done
  k logs "$1" 2>&1
  [ "$phase" = Succeeded ] || { echo "pod $1: ${phase:-no phase}"; return 1; }
}
pod_says() { # <name> <image> <command> <expected output>
  local out
  out=$(run_pod "$1" "$2" "$3") && grep -q -- "$4" <<< "$out" || { echo "$out"; return 1; }
}

command -v kind >/dev/null && command -v kubectl >/dev/null || { echo "needs kind and kubectl: brew install kind kubectl" >&2; exit 1; }
"$ORBIT" k8s down "$NAME" >/dev/null 2>&1
sleep 20; m_before=$(mib)

check "orbit k8s up (node Ready)" "$ORBIT" k8s up "$NAME"
check "kubectl from the Mac" k get nodes
check "system pods running" bash -c "! kubectl --context kind-$NAME get pods -A --no-headers | grep -v Running"
sleep 60; m_idle=$(mib)

check "deployment pulls from a registry" bash -c "kubectl --context kind-$NAME create deployment web --image=nginx:1.27-alpine >/dev/null && kubectl --context kind-$NAME rollout status deploy/web --timeout=180s"
k expose deploy/web --port=80 >/dev/null 2>&1
check "service DNS from a pod" pod_says dns alpine:3.22 'wget -qO- -T5 http://web.default.svc.cluster.local' nginx
check "pod HTTPS, Alpine (Mac roots)" pod_says tls-alpine alpine:3.22 'wget -q -T10 -O /dev/null https://dl-cdn.alpinelinux.org/ && echo ok' ok
check "pod HTTPS, UBI (symlinked bundle)" pod_says tls-ubi registry.access.redhat.com/ubi9/ubi-minimal:latest 'curl -sS -o /dev/null https://dl-cdn.alpinelinux.org/ && echo ok' ok
check "pod HTTPS, Node.js fetch" pod_says tls-node node:22-alpine "node -e \"fetch('https://registry.npmjs.org/').then(r => console.log(r.status))\"" 200
check "persistent volume (local-path)" bash -c "cat <<'Y' | kubectl --context kind-$NAME apply -f - >/dev/null && kubectl --context kind-$NAME wait --for=jsonpath='{.status.phase}'=Succeeded pod/pvc --timeout=180s >/dev/null && kubectl --context kind-$NAME logs pvc | grep -q stored
apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: data}
spec: {accessModes: [ReadWriteOnce], resources: {requests: {storage: 64Mi}}}
---
apiVersion: v1
kind: Pod
metadata: {name: pvc}
spec:
  restartPolicy: Never
  containers: [{name: c, image: alpine:3.22, command: [sh, -c, 'echo stored > /d/f && cat /d/f'], volumeMounts: [{name: d, mountPath: /d}]}]
  volumes: [{name: d, persistentVolumeClaim: {claimName: data}}]
Y"
check "locally built image (kind load)" bash -c "printf 'FROM alpine:3.22\nCMD [\"echo\",\"local-image\"]\n' | docker build -q -t orbittest/k8s-local:1 - >/dev/null && kind load docker-image orbittest/k8s-local:1 --name $NAME >/dev/null && kubectl --context kind-$NAME run local --image=orbittest/k8s-local:1 --image-pull-policy=Never --restart=Never >/dev/null && kubectl --context kind-$NAME wait --for=jsonpath='{.status.phase}'=Succeeded pod/local --timeout=60s >/dev/null && kubectl --context kind-$NAME logs local | grep -q local-image"
check "port-forward to the Mac" bash -c "kubectl --context kind-$NAME port-forward deploy/web 18089:80 >/dev/null 2>&1 & pf=\$!; sleep 3; curl -s -m5 http://127.0.0.1:18089 | grep -q nginx; rc=\$?; kill \$pf; exit \$rc"

t0=$(now)
check "20 pods Ready" bash -c "kubectl --context kind-$NAME scale deploy/web --replicas=20 >/dev/null && kubectl --context kind-$NAME rollout status deploy/web --timeout=240s"
t_scale=$(secs "$t0" "$(now)")
m_busy=$(mib)

check "orbit k8s down" "$ORBIT" k8s down "$NAME"
sleep 60; m_after=$(mib)
docker rmi orbittest/k8s-local:1 >/dev/null 2>&1
rm -f "$KUBECONFIG"
report INFO "VM memory on the Mac (MiB)" "before=$m_before cluster-idle=$m_idle 20-pods=$m_busy after-delete+60s=$m_after; scale-to-20=${t_scale}s"
echo "== k8s: $pass pass, $fail fail"
[ $fail = 0 ]
