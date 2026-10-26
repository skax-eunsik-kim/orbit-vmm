# Developer entry points. `make help` lists them.
SHELL := /bin/bash
DOCKER_SOCK := unix://$(HOME)/.orbit/vmm/docker.sock
SH_FILES := $(shell git ls-files '*.sh' scripts/orbit guest/init 'guest/etc/orbit/*' 'vmm/guest/init-*' | grep -v '\.py$$' | sort -u)

.PHONY: help guest build install up down update rollback test lint fmt devflow clean

help:
	@echo "make guest    download + build the guest kernel/initramfs (~/.orbit/guest)"
	@echo "make build    build and sign vmm/target/release/orbit-vmm"
	@echo "make install  build, copy the VMM and scripts to ~/.orbit/bin (outside shared dirs), install the watchdog"
	@echo "make up       orbit up: (re)start the VM from ~/.orbit/bin (builds nothing)"
	@echo "make down     shut the VM down (the watchdog won't restart it)"
	@echo "make update   install the latest release (TO=<tag>; NOW=1 restarts even with containers running)"
	@echo "make rollback go back to the version installed before the last update"
	@echo "make lint     cargo fmt --check, clippy -D warnings, shellcheck"
	@echo "make test     cargo test"
	@echo "make devflow  run the developer-workflow checks against the running VM"

guest:
	./scripts/fetch-guest.sh

build:
	$(MAKE) -C vmm build

install: build
	./scripts/install.sh

# These run only the installed `orbit` command: the checkout may be in a
# shared directory that containers can write to.
ORBIT := $(HOME)/.orbit/bin/orbit
NEED_ORBIT = @test -x $(ORBIT) || { echo "run make install first"; exit 1; }
up:
	$(NEED_ORBIT)
	$(ORBIT) up
down:
	$(NEED_ORBIT)
	$(ORBIT) down
update:
	$(NEED_ORBIT)
	$(ORBIT) update $(if $(TO),--to $(TO)) $(if $(NOW),--now)
rollback:
	$(NEED_ORBIT)
	$(ORBIT) rollback $(if $(NOW),--now)

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
