lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- --deny warnings

.PHONY: bump

bump:
	./scripts/bump_minor_release.sh
