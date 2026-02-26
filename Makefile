.PHONY: test test-bats test-parallel install lint help

JOBS ?= 8

help: ## Show this help
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | awk -F ':.*## ' '{printf "  %-16s %s\n", $$1, $$2}'

test: test-bats ## Run all tests (bats + parallel)

test-bats: ## Run bats test suite
	bats test/lit.bats

test-parallel: ## Run parallel stress test (JOBS=N, default 8)
	test/test.sh $(JOBS)

install: ## Symlink bin/lit to /usr/local/bin
	ln -sf $(CURDIR)/bin/lit /usr/local/bin/lit

lint: ## Shellcheck
	shellcheck bin/lit

clean: ## Clear test cache
	rm -rf test/cache/*
