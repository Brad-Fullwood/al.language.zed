# Zed AL Extension — Build System
#
# Usage:
#   make install   — first-time setup: build everything + symlink into PATH + Zed
#   make build     — rebuild everything (all Rust crates + WASM extension + .NET bridges)
#   make rust      — rebuild all Rust crates (native, excludes zed-al)
#   make wasm      — rebuild only the WASM extension (zed-al, wasm32-wasip2)
#   make bridges   — rebuild just .NET bridges
#   make grammar   — regenerate the tree-sitter-al parser sources before a
#                    fresh `tree-sitter build`
#   make language  — regenerate only the Zed-facing languages/al package files
#   make record-methods — regenerate the Microsoft-derived Record method catalog
#   make check-record-methods — prove the catalog matches the pinned Microsoft DLL
#   make repro-artifacts — regenerate generated artifacts; fail on any diff (CI drift guard)
#   make live-bc-contracts — strict tenant-backed publish/DAP/test/snapshot profile
#   make release-dryrun  — read-only release-readiness gate (never publishes)
#   make clean     — clean all build artifacts

SHELL := /bin/bash
ROOT := $(shell pwd)
LSP_BIN := $(ROOT)/target/debug/al-lsp
EXPLORER_BIN := $(ROOT)/target/debug/al-explorer
INSTALL_DIR := $(HOME)/.local/bin
ZED_EXT_DIR := $(HOME)/.local/share/zed/extensions/installed

# .NET bridge projects (quoted for paths with spaces)
ALSEMANTIC_PROJ := "$(ROOT)/crates/al-semantic/bridge/AlBridge.csproj"
WASM_BIN := $(ROOT)/target/wasm32-wasip2/release/zed_al.wasm

.PHONY: build install install-lsp dev-setup watch rust wasm bridges grammar language record-methods check-record-methods repro-artifacts microsoft-contracts live-bc-contracts release-dryrun crates-publish-dryrun clean

# Crates that are NOT published to crates.io (publish = false): the root wasm
# extension plus the binary/harness crates. Everything else under crates/* is a
# publishable library crate.
PUBLISH_EXCLUDE := zed-al al-lsp al-explorer al-protocol al-test-harness

# ── Default: rebuild everything ──────────────────────────────────
build: rust wasm bridges
	@echo ""
	@echo "Build complete. Restart the LSP in Zed to pick up changes."

# ── First-time install ───────────────────────────────────────────
install: build
	@mkdir -p $(INSTALL_DIR)
	@# al-lsp is COPIED, not symlinked — see the install-lsp target for why a
	@# symlink into target/debug/ silently breaks semantic analysis. `build` ran
	@# `rust`, whose last step produced the --features semantic binary.
	@rm -f "$(INSTALL_DIR)/al-lsp"
	@cp -f "$(LSP_BIN)" "$(INSTALL_DIR)/al-lsp"
	@bdir=$$(ls -dt target/debug/build/al-semantic-*/out/bridge 2>/dev/null | head -1); \
	 if [ -n "$$bdir" ]; then rm -rf "$(INSTALL_DIR)/bridge"; cp -r "$$bdir" "$(INSTALL_DIR)/bridge"; fi
	@echo "Installed al-lsp (semantic, copied) -> $(INSTALL_DIR)/al-lsp"
	@if [ ! -L "$(INSTALL_DIR)/al-explorer" ] && [ ! -f "$(INSTALL_DIR)/al-explorer" ]; then \
		ln -sf "$(EXPLORER_BIN)" "$(INSTALL_DIR)/al-explorer"; \
		echo "Symlinked al-explorer -> $(INSTALL_DIR)/al-explorer"; \
	else \
		echo "al-explorer already in $(INSTALL_DIR) (OK)"; \
	fi
	@# `al` back-compat alias: ONLY create/claim it when the name is free or
	@# already ours. On machines with Microsoft's AL dotnet tool installed
	@# (a prerequisite for the toolchain!), `al` is Microsoft's altool
	@# wrapper — clobbering it would break alc discovery, and the previous
	@# blanket "(OK)" message claimed a foreign binary as our alias
	@# Use `al-explorer` in scripts; the `al` alias is best-effort.
	@if [ -L "$(INSTALL_DIR)/al" ] && [ "$$(readlink "$(INSTALL_DIR)/al")" = "$(EXPLORER_BIN)" ]; then \
		echo "al alias -> al-explorer already installed (OK)"; \
	elif [ ! -e "$(INSTALL_DIR)/al" ]; then \
		ln -sf "$(EXPLORER_BIN)" "$(INSTALL_DIR)/al"; \
		echo "Symlinked al -> $(INSTALL_DIR)/al (alias for al-explorer)"; \
	else \
		echo "NOTE: $(INSTALL_DIR)/al exists and is NOT our alias (likely Microsoft's AL dotnet tool)."; \
		echo "      Leaving it untouched — use 'al-explorer' for this project's CLI."; \
	fi
	@mkdir -p "$(ZED_EXT_DIR)"
	@if [ ! -L "$(ZED_EXT_DIR)/al" ] && [ ! -d "$(ZED_EXT_DIR)/al" ]; then \
		ln -sf "$(ROOT)" "$(ZED_EXT_DIR)/al"; \
		echo "Dev extension symlinked into Zed."; \
		echo "NOTE: Run 'zed: install dev extension' once from the command palette to activate the extension."; \
	else \
		echo "Dev extension already installed in Zed (OK)"; \
	fi
	@echo ""
	@echo "Install complete."

# ── Fast refresh of just the semantic al-lsp ─────────────────────
# Copy rather than symlink the feature-enabled binary: Cargo uses the same
# target path for default and semantic builds, so a later workspace build can
# otherwise replace the installed semantic binary with the stub build.
install-lsp:
	@echo "=== Rebuild + reinstall semantic al-lsp ==="
	cargo build -p al-lsp --bin al-lsp --features semantic
	@mkdir -p $(INSTALL_DIR)
	@rm -f "$(INSTALL_DIR)/al-lsp"
	@cp -f "$(LSP_BIN)" "$(INSTALL_DIR)/al-lsp"
	@bdir=$$(ls -dt target/debug/build/al-semantic-*/out/bridge 2>/dev/null | head -1); \
	 if [ -n "$$bdir" ]; then rm -rf "$(INSTALL_DIR)/bridge"; cp -r "$$bdir" "$(INSTALL_DIR)/bridge"; echo "  bridge: $$bdir"; fi
	@echo "Reinstalled semantic al-lsp -> $(INSTALL_DIR)/al-lsp"
	@echo "Restart the AL language server in Zed (or reopen the .al file) to load it."

# ── One-shot dev environment setup ───────────────────────────────
# Installs prerequisites then builds + symlinks everything. Run once
# on a fresh machine, then use `make watch` while developing.
dev-setup:
	@echo "=== Ensuring prerequisites ==="
	@rustup target add wasm32-wasip2 2>/dev/null || true
	@command -v cargo-watch >/dev/null 2>&1 || { echo "Installing cargo-watch..."; cargo install cargo-watch; }
	@case ":$$PATH:" in *":$(INSTALL_DIR):"*) ;; *) echo "NOTE: $(INSTALL_DIR) is not on your PATH — add it so al-lsp/al-explorer are found.";; esac
	@$(MAKE) install
	@echo ""
	@echo "Dev setup complete. Run 'make watch' to auto-rebuild binaries on every change."

# ── Auto-rebuild on change (always-latest binaries for testing) ──
# Keeps al-lsp, al-explorer and the WASM extension rebuilt as you edit.
# Binaries are symlinked, so rebuilds are picked up with no reinstall.
#   make watch              debug native build (fast rebuilds)
#   make watch ARGS=--release   release native build
watch:
	@bash scripts/dev-watch.sh $(ARGS)

# ── Build ALL Rust workspace crates ──────────────────────────────
# al-lsp is built with `--features semantic` so it includes the real in-process
# .NET CLR host (Microsoft.Dynamics CodeAnalysis bridge). Without it, the binary
# links the no-op stub host and every semantic request fails with "Bridge not
# initialized". The rest of the workspace builds without the feature.
rust:
	@echo "=== Building all Rust crates ==="
	cargo build --workspace --exclude zed-al
	@echo "=== Building al-lsp with semantic (.NET CLR) support ==="
	cargo build -p al-lsp --bin al-lsp --features semantic

# ── Build WASM extension ─────────────────────────────────────────
wasm:
	@echo "=== Building WASM extension (zed-al) ==="
	cargo build -p zed-al --target wasm32-wasip2 --release
	@bash scripts/check-zed-wasm-component.sh "$(WASM_BIN)"
	@echo "  zed-al: $(WASM_BIN)"

# ── Build .NET bridges ───────────────────────────────────────────
bridges:
	@echo "=== Building .NET bridges ==="
	@if [ -f $(ALSEMANTIC_PROJ) ]; then \
		dotnet build $(ALSEMANTIC_PROJ) --nologo -v quiet && echo "  AlSemantic: OK"; \
	fi

# ── Regenerate tree-sitter-al parser ─────────────────────────────
# `tree-sitter build --output target/tree-sitter-al.so` from inside
# tree-sitter-al/ fails on a fresh clone because src/grammar.json,
# src/parser.c, src/node-types.json and friends are gitignored in the
# submodule. Run this target first to materialise them; it shells out
# to the al-gen generator, then `tree-sitter generate`.
#
# Requires: tree-sitter CLI on PATH, plus the al-extract step's inputs
# (Microsoft VS Code AL extension assets — see tree-sitter-al/README.md).
grammar:
	@echo "=== Regenerating tree-sitter-al parser sources ==="
	@if [ ! -d tree-sitter-al/generator ]; then \
		echo "ERROR: tree-sitter-al submodule is empty. Run \`git submodule update --init --recursive\` first."; \
		exit 1; \
	fi
	@if ! command -v tree-sitter >/dev/null; then \
		echo "ERROR: tree-sitter CLI not found on PATH. Install: cargo install tree-sitter-cli"; \
		exit 1; \
	fi
	tree-sitter-al/tests/check_cli_version.sh
	cd tree-sitter-al/generator && cargo run --release --bin al-gen
	cd tree-sitter-al && tree-sitter generate
	@echo ""
	@echo "Parser sources regenerated. You can now run:"
	@echo "  cd tree-sitter-al && tree-sitter build --output target/tree-sitter-al.so"

# ── Regenerate only the Zed language package ─────────────────────
# This is intentionally lighter than `make grammar`: it does not need the
# Microsoft AL extension or tree-sitter CLI. Canonical tree-sitter queries are
# copied from tree-sitter-al/queries, and Zed-specific language metadata is
# copied from generator-owned templates under tree-sitter-al/generator.
language:
	@echo "=== Regenerating languages/al from al-gen ==="
	cd tree-sitter-al/generator && cargo run --release --bin al-gen -- --zed-language-only
	@echo "Zed language package regenerated."

# ── Regenerate the Microsoft-derived Record method catalog ───────
record-methods:
	@test -n "$(AL_TOOL_PATH)" || { echo "ERROR: set AL_TOOL_PATH to the Microsoft AL extension's bin/<platform> directory."; exit 2; }
	@test -f "$(AL_TOOL_PATH)/Microsoft.Dynamics.Nav.CodeAnalysis.dll" || { echo "ERROR: Microsoft.Dynamics.Nav.CodeAnalysis.dll not found below AL_TOOL_PATH."; exit 2; }
	cargo run -p al-semantic --features semantic --example export_record_methods -- \
		"$(AL_TOOL_PATH)/Microsoft.Dynamics.Nav.CodeAnalysis.dll" \
		"$(ROOT)/crates/al-syntax/data/record_methods.json"

# Generate beside the checkout and compare bytes so verification neither
# rewrites the working tree nor mistakes another generated-file change for a
# current Record catalog.
check-record-methods:
	@test -n "$(AL_TOOL_PATH)" || { echo "UNAVAILABLE: set AL_TOOL_PATH to the Microsoft AL extension's bin/<platform> directory."; exit 2; }
	@test -f "$(AL_TOOL_PATH)/Microsoft.Dynamics.Nav.CodeAnalysis.dll" || { echo "UNAVAILABLE: Microsoft.Dynamics.Nav.CodeAnalysis.dll not found below AL_TOOL_PATH."; exit 2; }
	@tmp_file=$$(mktemp "$${TMPDIR:-/tmp}/al-record-methods.XXXXXX.json") || exit 1; \
	trap 'rm -f -- "$$tmp_file"' EXIT; \
	if ! cargo run -p al-semantic --features semantic --example export_record_methods -- \
		"$(AL_TOOL_PATH)/Microsoft.Dynamics.Nav.CodeAnalysis.dll" "$$tmp_file"; then \
		echo "ERROR: Record method catalog generation failed."; \
		exit 1; \
	fi; \
	if cmp -s "$(ROOT)/crates/al-syntax/data/record_methods.json" "$$tmp_file"; then \
		echo "Record method catalog matches the Microsoft AL toolchain."; \
	else \
		echo "DRIFT: crates/al-syntax/data/record_methods.json does not match the Microsoft AL toolchain."; \
		diff -u "$(ROOT)/crates/al-syntax/data/record_methods.json" "$$tmp_file" | head -120; \
		exit 1; \
	fi

# ── Reproducible generated artifacts (CI drift guard) ────────────
# Prove the committed generated outputs regenerate with NO diff. On success the
# working tree stays clean (regeneration is byte-identical); on drift it exits
# non-zero and leaves the regenerated files for inspection — run `make language`
# and commit. gen-zed-index has no committed baseline in this repo (the
# editor-e2e harness generates it on demand into target/), so the index is only
# checked for determinism; the committed-artifact diff target is languages/al.
repro-artifacts:
	@echo "=== Reproducible-artifact check ==="
	@echo "--- regenerate languages/al + diff (make language) ---"
	@$(MAKE) --no-print-directory language
	@if git diff --quiet -- languages/; then \
		echo "  languages/al: reproducible (matches committed)"; \
	else \
		echo "DRIFT: languages/al differs from generator output — run 'make language' and commit:"; \
		git --no-pager diff --stat -- languages/; \
		exit 1; \
	fi
	@echo "--- gen-zed-index determinism ---"
	@mkdir -p target
	@cargo run -q -p al-test-harness --bin gen-zed-index -- "$(ROOT)" > target/zed-index.a.json
	@cargo run -q -p al-test-harness --bin gen-zed-index -- "$(ROOT)" > target/zed-index.b.json
	@if diff -q target/zed-index.a.json target/zed-index.b.json >/dev/null; then \
		echo "  zed-index.json: deterministic"; \
	else \
		echo "NONDETERMINISTIC: gen-zed-index output varies between runs"; \
		diff -u target/zed-index.a.json target/zed-index.b.json | head -40; \
		exit 1; \
	fi
	@echo "Reproducible-artifact check passed."

# ── Microsoft toolchain contract profile ─────────────────────────
# This profile is deliberately strict: an absent compiler, bridge, or coherent
# dependency package cache is UNAVAILABLE/non-zero, never a successful skipped
# test. The individual Rust tests are `#[ignore]` in self-contained runs and
# execute only through this explicit external-contract gate.
microsoft-contracts:
	@echo "=== Microsoft AL toolchain contract profile ==="
	@if [ -z "$$AL_TOOL_PATH" ] || [ ! -f "$$AL_TOOL_PATH/alc.dll" ] || [ ! -f "$$AL_TOOL_PATH/Microsoft.Dynamics.Nav.CodeAnalysis.dll" ]; then \
		echo "UNAVAILABLE: AL_TOOL_PATH must contain alc.dll and Microsoft.Dynamics.Nav.CodeAnalysis.dll"; \
		exit 2; \
	fi
	@if ! command -v dotnet >/dev/null 2>&1 || ! dotnet --version >/dev/null 2>&1; then \
		echo "UNAVAILABLE: a working dotnet host is required"; \
		exit 2; \
	fi
	@if [ -z "$$AL_PACKAGE_CACHE_PATH" ] || [ ! -d "$$AL_PACKAGE_CACHE_PATH" ]; then \
		echo "UNAVAILABLE: AL_PACKAGE_CACHE_PATH must name a coherent dependency package directory"; \
		exit 2; \
	fi
	@$(MAKE) --no-print-directory check-record-methods
	cargo build -p al-lsp --bin al-lsp --features semantic
	cargo test -p al-semantic --features semantic --test live_bridge -- --ignored --nocapture
	cargo test -p al-test-harness --test semantic_bridge -- --ignored --nocapture
	cargo test -p al-test-harness --test pack_native_validate -- --ignored --nocapture
	cargo test -p al-test-harness --test emit_differential -- --ignored --nocapture
	cargo test -p al-test-harness --test zed_simulation builtin -- --include-ignored --nocapture --test-threads=1
	@echo "Microsoft AL toolchain contract profile passed."

# ── Live Business Central contract profile ───────────────────────
# Strict external-service gate. The default repository fixture derives the
# project/test/breakpoint contract; missing tenant/environment/version/token
# inputs are UNAVAILABLE/non-zero, and the ignored Rust integration test is
# never counted as passed by the self-contained suite.
live-bc-contracts:
	@bash scripts/live-bc-contracts.sh

# ── Release readiness dry-run (READ-ONLY, never publishes) ────────
# Runs every self-contained gate a release would, without uploading anything:
# grammar/generator/corpus/package checks, repository and generated-artifact
# invariants, formatting and clippy, native and WASM builds, host/WASM-extension
# tests, the full native workspace test suite, and local package-manifest audits.
# Profiles needing a proprietary Microsoft extension, `alc`, or live Business
# Central remain explicit environment-gated checks in Docs/testing-guide.md.
# The separate `crates-publish-dryrun` target is the strict registry-resolution
# gate and never converts a blocked dependency into success.
release-dryrun:
	@echo "=== Release dry-run (read-only; nothing is published) ==="
	@echo "--- 1/13 grammar crate tests ---"
	cd tree-sitter-al && cargo test --all-targets
	@echo "--- 2/13 grammar generator tests ---"
	cd tree-sitter-al && cargo test --manifest-path generator/Cargo.toml --all-targets
	@echo "--- 3/13 focused grammar fixtures + pinned repository corpus ---"
	tree-sitter-al/tests/run_repo_tests.sh
	@echo "--- 4/13 grammar package manifest ---"
	cd tree-sitter-al && cargo package --list
	@echo "--- 5/13 repo-slug consistency ---"
	@bash scripts/check-repo-consistency.sh
	@bash scripts/test-use-api.sh
	@echo "--- 6/13 stale crates/<name> doc references ---"
	@bash scripts/check-doc-paths.sh
	@echo "--- 7/13 release hygiene (versions / submodule / grammar rev / generated assets) ---"
	@bash scripts/check-release-hygiene.sh
	@echo "--- 8/13 reproducible generated artifacts ---"
	@$(MAKE) --no-print-directory repro-artifacts
	@echo "--- 9/13 formatting + clippy + benchmark harness contracts ---"
	cargo fmt --all -- --check
	cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings
	python3 -m unittest discover -s benchmarks/scripts -p 'test_*_bench.py'
	@echo "--- 10/13 build workspace (excl zed-al) + real semantic al-lsp ---"
	cargo build --workspace --exclude zed-al
	cargo build -p al-lsp --bin al-lsp --features semantic
	@echo "--- 11/13 test workspace (excl zed-al) ---"
	cargo test --workspace --exclude zed-al
	@echo "--- 12/13 test + build the actual Zed extension ---"
	cargo test -p zed-al
	cargo build -p zed-al --target wasm32-wasip2 --release
	@bash scripts/check-zed-wasm-component.sh "$(WASM_BIN)"
	@echo "--- 13/13 local package-manifest audit for publishable crates ---"
	@fail=0; \
	for dir in crates/*/; do \
		f="$$dir/Cargo.toml"; \
		[ -f "$$f" ] || continue; \
		name=$$(awk -F'"' '/^name[[:space:]]*=/{print $$2; exit}' "$$f"); \
		case " $(PUBLISH_EXCLUDE) " in *" $$name "*) continue;; esac; \
		grep -Eq '^publish[[:space:]]*=[[:space:]]*false' "$$f" && continue; \
		out=$$(cargo package --list --allow-dirty -p "$$name" 2>&1); \
		if [ $$? -eq 0 ]; then \
			echo "  $$name: OK (package manifest)"; \
		else \
			echo "  $$name: FAILED"; echo "$$out" | tail -8; fail=1; \
		fi; \
	done; \
	[ $$fail -eq 0 ] || { echo "release-dryrun: a crate package manifest is invalid"; exit 1; }
	@echo ""
	@echo "Release dry-run complete (read-only — nothing published)."

# Strict crates.io resolution gate. This is deliberately separate from the Zed
# extension release: before the first dependency-ordered library publication,
# registry resolution is expected to fail, and that failure must stay visible.
crates-publish-dryrun:
	@echo "=== Strict crates.io publish dry-run (read-only) ==="
	@fail=0; \
	for dir in crates/*/; do \
		f="$$dir/Cargo.toml"; \
		[ -f "$$f" ] || continue; \
		name=$$(awk -F'"' '/^name[[:space:]]*=/{print $$2; exit}' "$$f"); \
		case " $(PUBLISH_EXCLUDE) " in *" $$name "*) continue;; esac; \
		grep -Eq '^publish[[:space:]]*=[[:space:]]*false' "$$f" && continue; \
		if cargo publish --dry-run --no-verify -p "$$name"; then \
			echo "  $$name: OK (registry dry-run)"; \
		else \
			echo "  $$name: FAILED/BLOCKED"; fail=1; \
		fi; \
	done; \
	[ $$fail -eq 0 ] || { echo "crates-publish-dryrun: registry readiness is not green"; exit 1; }

# ── Clean ────────────────────────────────────────────────────────
clean:
	cargo clean
	@if [ -f $(ALSEMANTIC_PROJ) ]; then dotnet clean $(ALSEMANTIC_PROJ) --nologo -v quiet 2>/dev/null; fi
	@echo "Clean complete."
