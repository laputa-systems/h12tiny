.PHONY: test lint publish publish-mycrates

test:
	cargo test --locked --workspace --all-features

lint:
	cargo clippy --locked --workspace --all-targets --all-features -- --deny warnings

publish:
	./scripts/bump_minor_release.sh
	$(MAKE) publish-mycrates

# Publish to our private registry as well. `cargo publish --registry`
# resolves rewritten path dependencies against crates.io, so only crates
# whose path-only deps exist there can ship: the remaining workspace
# members (core, client, server, web, facade) need
# laputa-hyper-futures-lite =1.12.0-beta.1, which is not on crates.io.
STUFF ?= stuff
REGISTRY ?= mycrates
CRATES ?= h12tiny-client-normalize h12tiny-tls-system h12tiny-util h12tiny-client-sync
publish-mycrates:
	$(STUFF) publish cargo $(addprefix -p ,$(CRATES)) --registry $(REGISTRY)
