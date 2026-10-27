# bench

Scripts that measure orbit-vmm (and, for comparison, other engines) on a
running VM. CI can't boot a VM, so these are run locally; paste their result
lines into PRs. Numbers from them are in [docs/benchmarks.md](../docs/benchmarks.md).

| Path | What it measures |
|---|---|
| [`devflow/devflow.sh`](devflow/devflow.sh) | Developer-workflow checks: compose up, ports, bind mounts and inotify, HTTPS behind a TLS proxy, amd64 images, BuildKit builds, CA bundle handling, clock skew. Expect 0 fail. |
| [`devflow/compose.yml`](devflow/compose.yml), [`devflow/app/`](devflow/app) | The stack and sample app `devflow.sh` uses (project `orbittest`, so it runs next to real stacks). |
| [`memtest.sh`](memtest.sh) | The VM's `phys_footprint` idle, during a container that dirties ~3.5 GB, and 30 s / 90 s after it exits. |
| [`memfrag.sh`](memfrag.sh) | How much freed but fragmented guest memory stays on the Mac. |
| [`bench.sh`](bench.sh) | Container start latency, bind-mount and in-VM disk I/O, published-port latency, and the VM's memory. |
| [`k8s/k8s.sh`](k8s/k8s.sh) | `orbit k8s`: cluster up, pulls, service DNS, pod HTTPS through the Mac's roots (Alpine, UBI, Node.js), volumes, `kind load`, port-forward, 20 pods; the VM's memory before, with the cluster, and after deleting it. Expect 0 fail. |
| [`stress/multi.sh`](stress/multi.sh) | N worktrees, each with its own compose stack, in one VM at once. |
| [`stress/soak.sh`](stress/soak.sh) | Long-running container and file churn, sampling health and resource metrics. |

`devflow.sh`, `bench.sh`, `memtest.sh` and `stress/multi.sh` take a Docker
host or context (see each script's usage line), so the same run can be pointed
at an isolated instance:
`bench/devflow/devflow.sh x -H unix://$HOME/.orbit/vmm-x/docker.sock`.
`memfrag.sh` and `stress/soak.sh` always use the default VM (`~/.orbit/vmm`).
`k8s/k8s.sh` takes the `orbit` command to use and follows `ORBIT_INSTANCE`:
`ORBIT_INSTANCE=x bench/k8s/k8s.sh ~/.orbit-x/bin/orbit`.
VMM-level tests that boot their own tiny guests are in [`vmm/bench/`](../vmm/bench).
