.PHONY: check test migrate api worker

check:
	cargo fmt --all --check
	cargo check --all-targets --locked
	cargo clippy --all-targets --all-features --locked -- -D warnings

test:
	cargo test --all-targets --locked

migrate:
	cargo run --locked --bin commit-migrate

api:
	cargo run --locked --bin commit-api

worker:
	cargo run --locked --bin commit-worker
