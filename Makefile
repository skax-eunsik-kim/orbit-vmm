# Developer entry points. `make help` lists them.
SHELL := /bin/bash
DOCKER_SOCK := unix://$(HOME)/.orbit/vmm/docker.sock
SH_FILES := $(shell git ls-files '*.sh' guest/init 'guest/etc/orbit/*' 'vmm/guest/init-*' | grep -v '\.py$$' | sort -u)

.PHONY: help guest build up down test lint fmt devflow clean

help:
	@echo "make guest    download + build the guest kernel/initramfs (~/.orbit/guest)"
	@echo "make build    build and sign vmm/target/release/orbit-vmm"
	@echo "make up       (re)start the VM"
	@echo "make down     shut the VM down"
	@echo "make lint     cargo fmt --check, clippy -D warnings, shellcheck"
	@echo "make test     cargo test"
	@echo "make devflow  run the developer-workflow checks against the running VM"

guest:
	./scripts/fetch-guest.sh

build:
	$(MAKE) -C vmm build

up:
	./scripts/vmm-up.sh

down:
	@echo shutdown | nc -U -w 3 $(HOME)/.orbit/vmm/control.sock || true

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
