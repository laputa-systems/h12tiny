.PHONY: test lint publish

test:
	cargo test --locked --workspace --all-features

lint:
	cargo clippy --locked --workspace --all-targets --all-features -- --deny warnings

publish:
	./scripts/bump_minor_release.sh
