.PHONY: build test test-unit test-bats test-parallel install clean help

JOBS ?= 8

help: ## Show this help
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | awk -F ':.*## ' '{printf "  %-16s %s\n", $$1, $$2}'

build: ## Build Rust binary (debug)
	cargo build
	mkdir -p bin
	ln -sf ../target/debug/lit bin/lit

release: ## Build Rust binary (release, optimized)
	cargo build --release
	mkdir -p bin
	ln -sf ../target/release/lit bin/lit

test: test-unit test-bats ## Run all tests (unit + bats)

test-unit: ## Run Rust unit tests
	cargo test $(if $(TEST_LOG),> '$(TEST_LOG)' 2>&1)

test-supplements-cli: build ## Verify supplementary acquisition CLI registration
	sh test/supplements-cli.sh $(CURDIR)/target/debug/lit

supplements-discover: build ## Discover declared supplements (DOI=..., optional PAGE=PMC article URL)
	./target/debug/lit download '$(DOI)' --supplements --url-only --json $(if $(CAPTURE),--capture-discovery '$(CAPTURE)') $(if $(PAGE),--supplements-page '$(PAGE)') $(if $(DISCOVERY),--from-discovery '$(DISCOVERY)')

supplements-download: build ## Acquire declared supplements (DOI=..., optional PAGE=PMC article URL)
	./target/debug/lit download '$(DOI)' --supplements --json $(if $(PAGE),--supplements-page '$(PAGE)') $(if $(DISCOVERY),--from-discovery '$(DISCOVERY)')

test-supplements: ## Run supplementary artifact regression tests
	cargo test supplements $(if $(TEST_LOG),> '$(TEST_LOG)' 2>&1)

REVIEW_PATCH ?= tmp/supplements-reviewed-ea364845.patch
supplements-save-reviewed-patch: ## Preserve the current reviewed staged patch before replacing it
	test ! -e '$(REVIEW_PATCH)'
	git diff --cached --binary --full-index > '$(REVIEW_PATCH)'

test-publisher-transport: ## Run deterministic scoped publisher transport tests
	cargo test publisher_transport $(if $(TRANSPORT_LOG),> '$(TRANSPORT_LOG)' 2>&1)

publisher-test-certificate: ## Regenerate the isolated publisher transport test certificate
	openssl req -x509 -newkey rsa:2048 -nodes -config test/fixtures/frontiers/test-cert.cnf -keyout test/fixtures/frontiers/test-key.pem -out test/fixtures/frontiers/test-cert.pem -days 36500

supplements-inspect-package: ## List captured declared EPUB package (CAPTURE=...)
	unzip -l '$(CAPTURE)/article.epub'
	unzip -p '$(CAPTURE)/article.epub' META-INF/container.xml OPS/package.opf

test-bats: build ## Run bats integration tests against the debug binary just built
	LIT=$(CURDIR)/target/debug/lit bats test/lit.bats

test-parallel: ## Run parallel stress test (JOBS=N, default 8)
	test/test.sh $(JOBS)

install: release ## Install to /usr/local/bin
	cp target/release/lit /usr/local/bin/lit

clean: ## Remove build artifacts and test cache
	cargo clean
	rm -rf test/cache/*
