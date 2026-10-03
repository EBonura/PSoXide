.DEFAULT_GOAL := help
include tools/sdk-examples.mk

.PHONY: help check test miri fmt lint example disc hello-tri hello-tri-disc run-tri
help:
	@echo "make check | test | miri | lint | hello-tri-disc"
	@echo "make disc EXAMPLE=hello-input; make run-tri FRONTEND=/path/to/frontend"
check:
	cargo check --locked --workspace --all-features
	cargo check --locked --manifest-path sdk/Cargo.toml --workspace --all-features
test:
	cargo test --locked --workspace
	cargo test --locked --manifest-path sdk/Cargo.toml --workspace
# Miri over psx-gpu's host tests, under both borrow models: its frame,
# ordering-table and stream code hands raw addresses to the DMA model.
# Provenance is permissive because a DMA link is a 24-bit integer on the
# console; leaks are ignored because the stream tests leak their 'static
# buffers on purpose. Needs `rustup component add miri`.
MIRI_FLAGS := -Zmiri-permissive-provenance -Zmiri-ignore-leaks
miri:
	MIRIFLAGS="$(MIRI_FLAGS)" cargo miri test --locked --manifest-path sdk/Cargo.toml -p psx-gpu
	MIRIFLAGS="$(MIRI_FLAGS) -Zmiri-tree-borrows" cargo miri test --locked --manifest-path sdk/Cargo.toml -p psx-gpu
fmt:
	cargo fmt --all
	cargo fmt --manifest-path sdk/Cargo.toml --all
lint:
	cargo run --locked -q -p xtask -- check-mfc0 sdk
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cargo clippy --locked --manifest-path sdk/Cargo.toml --workspace --all-targets --all-features -- -D warnings
	# Again for the guest: the cfg(target_arch = "mips") MMIO, DMA and asm paths
	# only exist there. Libraries only; the tests need std.
	cargo clippy --locked --manifest-path sdk/Cargo.toml --workspace --target $(TARGET) \
		-Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --all-features -- -D warnings
