# Contributing to orbit-vmm

Thanks for your interest. Bug reports, fixes, benchmarks on other Macs, and
documentation improvements are all welcome.

## Before you start

- For anything bigger than a small fix, open an issue first so we can agree on
  the approach.
- Check that your change does not raise memory use. Keeping memory low is the
  point of the project.
- The VMM deliberately has few dependencies. Adding a crate needs a good
  reason.

## Development setup

You need an Apple Silicon Mac on macOS 15 or later.

```bash
brew install rust libslirp shellcheck docker docker-compose
git clone https://github.com/<you>/orbit-vmm && cd orbit-vmm
make guest install up
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
```

`make install` copies your build to `~/.orbit/bin`, and `make up` runs the VM
from there. Keep the checkout inside a shared directory (`~/src`, `~/work`, …; see
`ORBIT_SHARE_ALLOW`), because the benchmark scripts bind-mount files from it.

Repository layout:

| Path | What |
|---|---|
| `vmm/` | The VMM (Rust). `src/vm/` is the machine, `src/virtio/` the devices. |
| `vmm/guest/` | Tiny test initramfs images (`init-*`) for booting single devices. |
| `guest/` | Files baked into the real guest: init, boot scripts, memd, runc wrapper. |
| `scripts/` | Guest image build, VM start, health check. |
| `bench/` | Workflow, memory, and stress benchmarks. |
| `docs/` | Architecture and benchmark notes. |

## Checks

`make lint test` runs what CI runs:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `shellcheck` on all shell scripts

CI can't boot VMs, because GitHub's macOS runners have no nested
virtualisation. Before opening a PR that touches the VMM or the guest, run the
relevant checks locally:

| Change | Run |
|---|---|
| Anything | `bench/devflow/devflow.sh orbit-vmm -H unix://$HOME/.orbit/vmm/docker.sock` (expect 21/21) |
| Memory / balloon / memd | `bench/memtest.sh orbit-vmm unix://$HOME/.orbit/vmm/docker.sock orbit-vmm` and `bench/memfrag.sh <label>` |
| virtio-fs | `vmm/bench/fssec.sh`, and `make -C vmm ~/.orbit/vmm/fstest.cpio.gz` then boot it |
| Concurrency / long runs | `bench/stress/multi.sh 4`, `bench/stress/soak.sh 1` |

Paste the result lines into the PR description.

## Style

- Rust: `rustfmt.toml` is the source of truth. Prefer plain std and `libc`
  over new crates. Wrap `unsafe` blocks tightly and say why they are sound.
- Shell: guest scripts run under busybox `ash`, so stay POSIX (pipefail is
  fine). Host scripts may use bash.
- Comments explain *why*, especially workarounds for macOS or guest quirks.
- Commit messages: `area: what changed` (for example
  `virtio/fs: honour O_APPEND`), with the reason in the body.

## Licensing

orbit-vmm is Apache-2.0. Contributions are accepted under the same licence
(inbound = outbound, Apache-2.0 §5). No CLA is required. Never commit built
guest images, data disks, or certificates.

## README translations

`README.md` is the source of truth. `README.<lang>.md` files are translations
(Korean, Japanese, Simplified Chinese, Spanish, Brazilian Portuguese, French,
German, Hindi), each with a note that the English version wins.

- When you change `README.md`, update the translations you can, or say in the
  PR which ones now lag. CI warns about each translation a PR leaves behind.
- Keep code blocks, commands, paths and numbers exactly as in `README.md`, and
  point in-page links at the translated heading's GitHub anchor.
- To add a language, add it to `LANGS` in `scripts/readme-langs.py` and run
  the script: it writes the language selector into every README.

