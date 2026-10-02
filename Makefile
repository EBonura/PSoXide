.DEFAULT_GOAL := help
include tools/sdk-examples.mk

.PHONY: help check test fmt lint example disc hello-tri hello-tri-disc run-tri
help:
	@echo "make check | test | lint | hello-tri-disc"
	@echo "make disc EXAMPLE=hello-input; make run-tri FRONTEND=/path/to/frontend"
check:
	cargo check --locked --workspace --all-features
	cargo check --locked --manifest-path sdk/Cargo.toml --workspace --all-features
test:
	cargo test --locked --workspace
	cargo test --locked --manifest-path sdk/Cargo.toml --workspace
fmt:
	cargo fmt --all
	cargo fmt --manifest-path sdk/Cargo.toml --all
lint:
	cargo run --locked -q -p xtask -- check-mfc0 sdk
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cargo clippy --locked --manifest-path sdk/Cargo.toml --workspace --all-targets --all-features -- -D warnings
