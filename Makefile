# Developer entry points. `make help` lists them.
SHELL := /bin/bash
DOCKER_SOCK := unix://$(HOME)/.orbit/vmm/docker.sock
SH_FILES := $(shell git ls-files '*.sh' guest/init 'guest/etc/orbit/*' 'vmm/guest/init-*' | grep -v '\.py$$' | sort -u)

.PHONY: help guest build install up down test lint fmt devflow clean

help:
	@echo "make guest    download + build the guest kernel/initramfs (~/.orbit/guest)"
	@echo "make build    build and sign vmm/target/release/orbit-vmm"
	@echo "make install  build, copy the VMM and scripts to ~/.orbit/bin (outside shared dirs), install the watchdog"
	@echo "make up       (re)start the VM from ~/.orbit/bin (builds nothing)"
	@echo "make down     shut the VM down (the watchdog won't restart it)"
	@echo "make lint     cargo fmt --check, clippy -D warnings, shellcheck"
	@echo "make test     cargo test"
	@echo "make devflow  run the developer-workflow checks against the running VM"

guest:
	./scripts/fetch-guest.sh

build:
	$(MAKE) -C vmm build

install: build
	./scripts/install.sh

# Runs only the installed copy: the checkout may be in a shared directory that
# containers can write to, so restarting must not build or run anything from it.
up:
	@test -x $(HOME)/.orbit/bin/vmm-up.sh || { echo "run make install first"; exit 1; }
	$(HOME)/.orbit/bin/vmm-up.sh

down:
	$(HOME)/.orbit/bin/vmm-up.sh --stop

test:
	cd vmm && cargo test

lint:
	cd vmm && cargo fmt --check
	cd vmm && cargo clippy --all-targets -- -D warnings
	shellcheck -S warning $(SH_FILES)

fmt:
	cd vmm && cargo fmt

devflow:
	bench/devflow/devflow.sh orbit-vmm -H $(DOCKER_SOCK)

clean:
	cd vmm && cargo clean
