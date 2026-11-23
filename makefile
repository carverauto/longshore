#
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026. Marvin Hansen. All Rights Reserved.
#

# Make will use bash instead of sh
SHELL := /usr/bin/env bash

.PHONY: help
help:
	@echo ''
	@echo '    make build   	Builds the code base incrementally (fast) for dev.'
	@echo '    make count   	Count LoC across the project.'
	@echo '    make fix   		Fixes linting issues as reported by clippy.'
	@echo '    make format   	Formats all code according to cargo fmt style.'
	@echo '    make update   	Update rust, pulls from git remote.'
	@echo '    make test   	Tests across all crates.'


.PHONY: build
build:
	@source scripts/build.sh

.PHONY: count
count:
	@source scripts/count.sh

.PHONY: fix
fix:
	@source scripts/fix.sh

.PHONY: format
format:
	@source scripts/format.sh

.PHONY: update
update:
	@source scripts/update.sh

.PHONY: test
test:
	@source scripts/test.sh