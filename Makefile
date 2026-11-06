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
WORK_DIR ?= .work/registry-pub
PUBLISH_PORT ?= 8080
STATIC_PORT ?= 8081
REGISTRY ?= mycrates
REG_ENV ?= MYCRATES
PUBLIC_URL ?= https://crates.17166969.xyz
BUCKET ?= stuff-mycrates
CRATES ?= h12tiny-client-normalize h12tiny-tls-system h12tiny-util h12tiny-client-sync
STUFF_DIR ?= ../stuff
publish-mycrates:
	@test -x "$(STUFF_DIR)/deploy/publish-mycrates.sh" || { echo "STUFF_DIR does not point at a stuff checkout: $(STUFF_DIR)" >&2; exit 2; }
	STUFF_DIR="$(STUFF_DIR)" \
	CRATES="$(CRATES)" \
	WORK_DIR="$(WORK_DIR)" PUBLISH_PORT="$(PUBLISH_PORT)" STATIC_PORT="$(STATIC_PORT)" \
	REGISTRY="$(REGISTRY)" REG_ENV="$(REG_ENV)" PUBLIC_URL="$(PUBLIC_URL)" BUCKET="$(BUCKET)" \
	"$(STUFF_DIR)/deploy/publish-mycrates.sh"
