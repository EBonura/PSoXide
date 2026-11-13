.DEFAULT_GOAL := help
include tools/sdk-examples.mk
include tools/engine-examples.mk

.PHONY: help check test miri fmt fmt-check lint example disc hello-tri hello-tri-disc run-tri
help:
	@echo "make check | test | miri | lint | repro-check | hello-tri-disc"
	@echo "make disc EXAMPLE=hello-input; make run-tri FRONTEND=/path/to/frontend"
check:
	cargo check --locked --workspace --all-features
	cargo check --locked --manifest-path sdk/Cargo.toml --workspace --all-features
	cargo check --locked --manifest-path engine/Cargo.toml --workspace --all-features
# psxed-project's tests read the editor repository's projects/ and assets/ (they run in
# PSoXide-editor, which hydrates this crate), so they are left out of the SDK run.
test:
	cargo test --locked --workspace --exclude psxed-project
	cargo test --locked --manifest-path sdk/Cargo.toml --workspace
	cargo test --locked --manifest-path engine/Cargo.toml -p psx-game-runtime -p psx-render-contract -p psx-chainloader -p psx-goldsrc -p psx-bsp
	cargo test --locked --manifest-path engine/Cargo.toml -p psx-level --features wire
	cargo test --locked --manifest-path engine/examples/editor-playtest/Cargo.toml
# Miri over every SDK crate's host tests and psx-hw's, under Stacked Borrows
# and then Tree Borrows (sdk/docs/MIRI.md). Provenance is permissive because a
# DMA link is a 24-bit integer on the console; leaks are ignored because tests
# leak their 'static buffers on purpose. Exhaustive sweeps carry
# #[cfg_attr(miri, ignore)] and run natively in `make test`. Needs
# `rustup component add miri`.
MIRI_FLAGS := -Zmiri-permissive-provenance -Zmiri-ignore-leaks
miri:
	MIRIFLAGS="$(MIRI_FLAGS)" cargo miri test --locked -p psx-hw
	MIRIFLAGS="$(MIRI_FLAGS)" cargo miri test --locked --manifest-path sdk/Cargo.toml --workspace --no-fail-fast
	MIRIFLAGS="$(MIRI_FLAGS) -Zmiri-tree-borrows" cargo miri test --locked -p psx-hw
	MIRIFLAGS="$(MIRI_FLAGS) -Zmiri-tree-borrows" cargo miri test --locked --manifest-path sdk/Cargo.toml --workspace --no-fail-fast
# The examples are their own workspaces, so `cargo fmt --all` does not reach them.
fmt:
	cargo fmt --all
	cargo fmt --manifest-path sdk/Cargo.toml --all
	cargo fmt --manifest-path engine/Cargo.toml --all
	for m in sdk/examples/*/Cargo.toml engine/examples/*/Cargo.toml; do cargo fmt --manifest-path "$$m" || exit 1; done
fmt-check:
	cargo fmt --all -- --check
	cargo fmt --manifest-path sdk/Cargo.toml --all -- --check
	cargo fmt --manifest-path engine/Cargo.toml --all -- --check
	for m in sdk/examples/*/Cargo.toml engine/examples/*/Cargo.toml; do cargo fmt --manifest-path "$$m" -- --check || exit 1; done
lint:
	tools/check-register-literals.sh
	sh tools/guest_symbol_gate_test.sh
	sh tools/wide_gate.sh
	cargo run --locked -q -p xtask -- check-mfc0 sdk engine
	cargo run --locked -q -p psoxide-dev -- lint-policy-guard
	cargo run --locked -q -p psoxide-dev -- runtime-numeric-guard
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cargo clippy --locked --manifest-path sdk/Cargo.toml --workspace --all-targets --all-features -- -D warnings
	cargo clippy --locked --manifest-path engine/Cargo.toml --workspace --all-targets --all-features -- -D warnings
	# Again for the guest: the cfg(target_arch = "mips") MMIO, DMA and asm paths
	# only exist there. Libraries only; the tests need std.
	cargo clippy --locked --manifest-path sdk/Cargo.toml --workspace --target $(TARGET) \
		-Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --all-features -- -D warnings
	tools/style-ratchet.sh
