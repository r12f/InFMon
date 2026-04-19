# InFMon top-level makefile.
# Thin wrappers so contributors only have to remember `make lint` / `make test`.
#
# See specs/001-ci-and-precommit.md for the contract.

.PHONY: help lint cppcheck-full test e2e build clean ci-branch-protection install-hooks

help:
	@echo "InFMon — make targets"
	@echo ""
	@echo "  make install-hooks         Install pre-commit + commit-msg hooks"
	@echo "  make lint                  Run all pre-commit hooks across the tree"
	@echo "  make cppcheck-full         Run cppcheck over the full C/C++ tree"
	@echo "  make test                  Run unit tests (Rust + C/C++); E2E NOT included"
	@echo "  make e2e                   Print the manual E2E procedure (run on r12f-bf3)"
	@echo "  make build                 Build all targets (Rust + C/C++)"
	@echo "  make clean                 Remove all build artefacts"
	@echo "  make ci-branch-protection  Apply branch protection to main (admin only)"

install-hooks:
	pre-commit install --install-hooks
	pre-commit install --hook-type commit-msg

lint:
	pre-commit run --all-files --show-diff-on-failure

cppcheck-full:
	@if [ -d src/backend ]; then \
	    suppr=""; \
	    if [ -f .cppcheck-suppressions ]; then \
	        suppr="--suppressions-list=.cppcheck-suppressions"; \
	    fi; \
	    cppcheck \
	        --enable=warning,performance,portability \
	        --error-exitcode=1 \
	        --inline-suppr \
	        $$suppr \
	        src/backend; \
	else \
	    echo "src/backend/ not present yet — nothing to check."; \
	fi

test:
	@echo "==> Rust workspace"
	@if [ -f Cargo.toml ]; then \
	    cargo test --workspace --all-features --locked; \
	else \
	    echo "    (no Cargo.toml yet — skipped)"; \
	fi
	@echo "==> C/C++ backend"
	@if [ -f src/backend/CMakeLists.txt ]; then \
	    cmake_launchers=""; \
	    if command -v ccache >/dev/null 2>&1; then \
	        cmake_launchers="-DCMAKE_C_COMPILER_LAUNCHER=ccache -DCMAKE_CXX_COMPILER_LAUNCHER=ccache"; \
	    fi; \
	    cmake -S src/backend -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug $$cmake_launchers \
	        && cmake --build build \
	        && ctest --test-dir build --output-on-failure; \
	else \
	    echo "    (no src/backend/CMakeLists.txt yet — skipped)"; \
	fi
	@echo ""
	@echo "NOTE: E2E tests are NOT run by 'make test'. They live under tests/ and"
	@echo "      must be executed manually on the bench machine (r12f-bf3) via"
	@echo "      'make e2e'. See specs/001-ci-and-precommit.md §6."

e2e:
	@echo "InFMon E2E tests run manually on r12f-bf3 (BlueField-3 bench machine)."
	@echo "They require SR-IOV, hugepages, and a loaded VPP plugin — none of which"
	@echo "are available in CI. See tests/README.md for the procedure."
	@false

build:
	@echo "==> Rust workspace"
	@if [ -f Cargo.toml ]; then \
	    cargo build --workspace --all-targets --locked; \
	else \
	    echo "    (no Cargo.toml yet — skipped)"; \
	fi
	@echo "==> C/C++ backend"
	@if [ -f src/backend/CMakeLists.txt ]; then \
	    cmake_launchers=""; \
	    if command -v ccache >/dev/null 2>&1; then \
	        cmake_launchers="-DCMAKE_C_COMPILER_LAUNCHER=ccache -DCMAKE_CXX_COMPILER_LAUNCHER=ccache"; \
	    fi; \
	    cmake -S src/backend -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug $$cmake_launchers \
	        && cmake --build build; \
	else \
	    echo "    (no src/backend/CMakeLists.txt yet — skipped)"; \
	fi

clean:
	rm -rf target/ build/ build-arm64/
	@echo "Cleaned target/, build/, build-arm64/"

ci-branch-protection:
	ci/branch-protection.sh
