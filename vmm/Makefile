BIN := target/release/orbit-vmm
OUT := $(HOME)/.orbit/vmm

.PHONY: build hello

build:
	cargo build --release
	codesign --force --sign - target/release/liborbitclose.dylib
	codesign --force --sign - --entitlements hvf.entitlements $(BIN)

$(OUT)/%.cpio.gz: guest/init-%
	@mkdir -p $(OUT)
	./guest/build-initramfs.sh $* $@

# Boot smoke test: kernel to userspace and back out through PSCI SYSTEM_OFF.
hello: build $(OUT)/hello.cpio.gz
	$(BIN) --kernel $(HOME)/.orbit/guest/kernel --initrd $(OUT)/hello.cpio.gz --mem 1024 --cpus 2
