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
	cargo test

test-bats: build ## Run bats integration tests
	bats test/lit.bats

test-parallel: ## Run parallel stress test (JOBS=N, default 8)
	test/test.sh $(JOBS)

install: release ## Install to /usr/local/bin
	cp target/release/lit /usr/local/bin/lit

clean: ## Remove build artifacts and test cache
	cargo clean
	rm -rf test/cache/*
