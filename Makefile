.PHONY: test lint publish

test:
	cargo test --locked

lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- --deny warnings

publish:
	./scripts/bump_minor_release.sh
