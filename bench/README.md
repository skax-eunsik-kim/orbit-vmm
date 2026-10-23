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
| [`stress/multi.sh`](stress/multi.sh) | N worktrees, each with its own compose stack, in one VM at once. |
| [`stress/soak.sh`](stress/soak.sh) | Long-running container and file churn, sampling health and resource metrics. |

Each script takes a label and a Docker host or context, so the same run can be
pointed at an isolated instance:
`bench/devflow/devflow.sh x -H unix://$HOME/.orbit/vmm-x/docker.sock`.
VMM-level tests that boot their own tiny guests are in [`vmm/bench/`](../vmm/bench).
