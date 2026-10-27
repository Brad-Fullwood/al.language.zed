# Zed AL Extension — Build System
#
# Usage:
#   make install   — build everything, copy al-lsp to the extension's work dir (dev/), verify, print status
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
#   make plugin-validate — check the Claude Code plugin under plugin/
#   make clean     — clean all build artifacts

SHELL := /bin/bash
ROOT := $(shell pwd)
# CARGO_TARGET_DIR moves cargo's output, so the install reads from it too.
TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(ROOT)/target)
LSP_BIN := $(TARGET_DIR)/debug/al-lsp
EXPLORER_BIN := $(TARGET_DIR)/debug/al-explorer
# al-explorer and the `al` alias for the terminal. al-lsp is copied here too
# for terminal use, but the extension does not look here.
INSTALL_DIR ?= $(HOME)/.local/bin
UNAME_S := $(shell uname -s)
# Zed's dev-extension directory is platform-specific: XDG data dir on Linux,
# Application Support on macOS (a supported dev platform per CI/README).
ifeq ($(UNAME_S),Darwin)
ZED_EXT_DIR ?= $(HOME)/Library/Application Support/Zed/extensions/installed
ZED_WORK_DIR ?= $(HOME)/Library/Application Support/Zed/extensions/work/al
else
ZED_EXT_DIR ?= $(HOME)/.local/share/zed/extensions/installed
# The extension's work directory: the extension resolves al-lsp from
# $(ZED_WORK_DIR)/dev first (see Docs/features/language-server.md).
ZED_WORK_DIR ?= $(HOME)/.local/share/zed/extensions/work/al
endif

# .NET bridge projects (quoted for paths with spaces)
ALSEMANTIC_PROJ := "$(ROOT)/crates/al-semantic/bridge/AlBridge.csproj"
WASM_BIN := $(TARGET_DIR)/wasm32-wasip2/release/zed_al.wasm

.PHONY: build install install-lsp install-files install-status dev-setup watch rust wasm bridges grammar language record-methods check-record-methods repro-artifacts shellcheck deny microsoft-contracts live-bc-contracts release-dryrun crates-publish-dryrun plugin-validate plugin-evals clean

# Crates that are NOT published to crates.io (publish = false): the root wasm
# extension plus the binary/harness crates. Everything else under crates/* is a
# publishable library crate.
PUBLISH_EXCLUDE := zed-al al-lsp al-explorer al-protocol al-test-harness

# ── Default: rebuild everything ──────────────────────────────────
build: rust wasm bridges
	@echo ""
	@echo "Build complete. Restart the LSP in Zed to pick up changes."

# ── Install ──────────────────────────────────────────────────────
# `install` builds everything, copies al-lsp into the extension's managed
# directory ($(ZED_WORK_DIR)/dev, the one place the extension looks), checks
# what it copied against `al-lsp --version`, links the dev extension into Zed
# and prints a status. It is idempotent.
install: build install-files install-status

# Fast refresh of just the semantic al-lsp: rebuild, copy, verify.
install-lsp:
	@echo "=== Rebuild + reinstall semantic al-lsp ==="
	cargo build -p al-lsp --bin al-lsp --features semantic
	@$(MAKE) --no-print-directory install-files
	@echo "Restart the AL language server in Zed (or reopen the .al file) to load it."

# Copies are made with `cp -p`: for a dirty tree `al-lsp --version` hashes the
# executable's size and mtime, so a copy must keep both to report the same
# build. al-lsp is COPIED, not symlinked: Cargo uses one target path for the
# default and the semantic build, so a later workspace build can replace a
# linked binary with the stub build. The bridge directory goes next to each
# copy, and al-explorer next to the managed al-lsp, which looks for it there.
install-files:
	@test -x "$(LSP_BIN)" || { echo "ERROR: $(LSP_BIN) missing, run 'make rust' first."; exit 1; }
	@mkdir -p "$(INSTALL_DIR)" "$(ZED_WORK_DIR)/dev"
	@bdir=$$(ls -dt "$(TARGET_DIR)"/debug/build/al-semantic-*/out/bridge 2>/dev/null | head -1); \
	 for dest in "$(INSTALL_DIR)" "$(ZED_WORK_DIR)/dev"; do \
	   rm -f "$$dest/al-lsp" "$$dest/al-explorer"; \
	   cp -p "$(LSP_BIN)" "$$dest/al-lsp"; \
	   if [ -x "$(EXPLORER_BIN)" ]; then cp -p "$(EXPLORER_BIN)" "$$dest/al-explorer"; fi; \
	   if [ -n "$$bdir" ]; then rm -rf "$$dest/bridge"; cp -rp "$$bdir" "$$dest/bridge"; fi; \
	 done
	@# `al` back-compat alias, a copy of al-explorer. It is claimed only when the
	@# name is free, is a symlink from an older install, or is already our
	@# binary. On machines with Microsoft's AL dotnet tool, `al` is Microsoft's
	@# altool wrapper and is left alone. Use `al-explorer` in scripts.
	@a="$(INSTALL_DIR)/al"; \
	 if [ ! -e "$$a" ] || [ -L "$$a" ] || cmp -s "$$a" "$(INSTALL_DIR)/al-explorer"; then \
	   rm -f "$$a"; cp -p "$(INSTALL_DIR)/al-explorer" "$$a"; \
	 else \
	   echo "NOTE: $$a is not our alias (likely Microsoft's AL dotnet tool), left untouched."; \
	 fi
	@# Stamp = what the installed binary prints, then check both copies say it.
	@stamp=$$("$(INSTALL_DIR)/al-lsp" --version) && [ -n "$$stamp" ] || { echo "ERROR: $(INSTALL_DIR)/al-lsp --version failed"; exit 1; }; \
	 printf '%s\n' "$$stamp" > "$(ZED_WORK_DIR)/dev/BUILD"; \
	 managed=$$("$(ZED_WORK_DIR)/dev/al-lsp" --version); \
	 built=$$("$(LSP_BIN)" --version); \
	 if [ "$$managed" != "$$stamp" ] || [ "$$(cat "$(ZED_WORK_DIR)/dev/BUILD")" != "$$managed" ]; then \
	   echo "ERROR: installed al-lsp reports '$$managed', expected '$$stamp'"; exit 1; fi; \
	 if [ "$$built" != "$$stamp" ]; then \
	   echo "ERROR: $(LSP_BIN) reports '$$built' but the copy reports '$$stamp'"; exit 1; fi
	@mkdir -p "$(ZED_EXT_DIR)"
	@if [ ! -L "$(ZED_EXT_DIR)/al" ] && [ ! -d "$(ZED_EXT_DIR)/al" ]; then \
		ln -sf "$(ROOT)" "$(ZED_EXT_DIR)/al"; \
	fi

# The short status `install` ends with. The dotnet check uses the rule the
# toolchain itself applies (crates/al-project/src/toolchain.rs dotnet_program):
# $AL_DOTNET_PATH, else `dotnet` on PATH. It is not a second lookup.
install-status:
	@echo ""
	@echo "al-lsp:       $(ZED_WORK_DIR)/dev/al-lsp  [$$(cat "$(ZED_WORK_DIR)/dev/BUILD")]"
	@echo "al-lsp (cli): $(INSTALL_DIR)/al-lsp"
	@echo "al-explorer:  $(INSTALL_DIR)/al-explorer"
	@echo "bridge:       $$([ -d "$(ZED_WORK_DIR)/dev/bridge" ] && echo yes || echo no)"
	@echo "dotnet:       $$("$${AL_DOTNET_PATH:-dotnet}" --version >/dev/null 2>&1 && echo yes || echo no)"
	@echo "wasm:         $$([ -f "$(WASM_BIN)" ] && echo built || echo missing)"
	@echo "grammar:      $$([ -f "$(ROOT)/grammars/al.wasm" ] && echo built || echo 'not built yet, Zed builds it when the dev extension installs')"
	@echo "dev ext link: $$([ -e "$(ZED_EXT_DIR)/al" ] && echo present || echo missing)"
	@echo ""
	@echo "Left to do in Zed: run 'zed: install dev extension' (first time) or 'zed: reload extensions'."

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
	committed="$(ROOT)/crates/al-syntax/data/record_methods.json"; \
	if cmp -s "$$committed" "$$tmp_file"; then \
		echo "Record method catalog matches the Microsoft AL toolchain."; \
	elif [ "$$(grep -v '"toolchainVersion"' "$$committed")" = "$$(grep -v '"toolchainVersion"' "$$tmp_file")" ]; then \
		echo "PROVENANCE DRIFT: the Record method set is identical, but AL_TOOL_PATH is a different toolchain build."; \
		echo "  committed toolchain: $$(sed -n 's/.*"toolchainVersion": "\(.*\)".*/\1/p' "$$committed")"; \
		echo "  measured toolchain:  $$(sed -n 's/.*"toolchainVersion": "\(.*\)".*/\1/p' "$$tmp_file")"; \
		echo "  Point AL_TOOL_PATH at the pinned toolchain, or run 'make record-methods' to move the pin deliberately."; \
		exit 1; \
	else \
		echo "DRIFT: crates/al-syntax/data/record_methods.json does not match the Microsoft AL toolchain."; \
		diff -u "$$committed" "$$tmp_file" | head -120; \
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

# ShellCheck over the same file set as the `test` job in .github/workflows/ci.yml.
# CI installs shellcheck; locally it may be absent, so say so rather than
# reporting a pass the tool never produced.
shellcheck:
	@if command -v shellcheck >/dev/null 2>&1; then \
		shellcheck \
			scripts/*.sh \
			plugin/scripts/*.sh \
			plugin/evals/*.sh \
			crates/al-test-harness/editor-e2e/*.sh \
			crates/al-test-harness/editor-e2e/container/*.sh \
			tree-sitter-al/tests/*.sh \
		&& echo "  shellcheck: OK"; \
	else \
		echo "  SKIPPED: shellcheck is not installed — CI still runs it (see ci.yml). Install it with your package manager."; \
	fi

# The deny.toml license, advisory, source and banned-crate policy, matching the
# `cargo-deny` job in .github/workflows/ci.yml. zed-al is included deliberately.
deny:
	@if command -v cargo-deny >/dev/null 2>&1; then \
		cargo deny --workspace check; \
	else \
		echo "  SKIPPED: cargo-deny is not installed — CI still runs it (see ci.yml). Install it with 'cargo install cargo-deny --locked'."; \
	fi

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
# invariants, ShellCheck, formatting and clippy, the cargo-deny dependency
# policy, native and WASM builds, host/WASM-extension tests, the full native
# workspace test suite, the semantic-feature clippy and tests, and local
# package-manifest audits. A tag push additionally waits for the whole CI
# workflow to conclude successfully (check-release-hygiene.sh --require-ci),
# so every gate that workflow runs is mirrored here.
# ShellCheck and cargo-deny are skipped with a printed note when the tool is
# not installed locally; CI installs both, so a skip here is not a pass there.
# Profiles needing a proprietary Microsoft extension, `alc`, or live Business
# Central remain explicit environment-gated checks in Docs/testing-guide.md.
# The separate `crates-publish-dryrun` target is the strict registry-resolution
# gate and never converts a blocked dependency into success.
release-dryrun:
	@echo "=== Release dry-run (read-only; nothing is published) ==="
	@echo "--- 1/16 grammar crate tests ---"
	cd tree-sitter-al && cargo test --all-targets
	@echo "--- 2/16 grammar generator tests ---"
	cd tree-sitter-al && cargo test --manifest-path generator/Cargo.toml --all-targets
	@echo "--- 3/16 focused grammar fixtures + pinned repository corpus ---"
	tree-sitter-al/tests/run_repo_tests.sh
	@echo "--- 4/16 grammar package manifest ---"
	cd tree-sitter-al && cargo package --list
	@echo "--- 5/16 repo-slug consistency ---"
	@bash scripts/check-repo-consistency.sh
	@bash scripts/test-use-api.sh
	@echo "--- 6/16 ShellCheck over the shipped shell scripts ---"
	@$(MAKE) --no-print-directory shellcheck
	@echo "--- 7/16 stale crates/<name> doc references ---"
	@bash scripts/check-doc-paths.sh
	@echo "--- 8/16 release hygiene (versions / submodule / grammar rev / generated assets) ---"
	@bash scripts/check-release-hygiene.sh
	@echo "--- 9/16 reproducible generated artifacts ---"
	@$(MAKE) --no-print-directory repro-artifacts
	@echo "--- 10/16 formatting + clippy + benchmark harness contracts ---"
	cargo fmt --all -- --check
	cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings
	cargo clippy -p zed-al --all-targets -- -D warnings
	python3 -m unittest discover -s benchmarks/scripts -p 'test_*_bench.py'
	@echo "--- 11/16 dependency policy (cargo-deny) ---"
	@$(MAKE) --no-print-directory deny
	@echo "--- 12/16 build workspace (excl zed-al) + real semantic al-lsp ---"
	cargo build --workspace --exclude zed-al
	cargo build -p al-lsp --bin al-lsp --features semantic
	@echo "--- 13/16 test workspace (excl zed-al) ---"
	cargo test --workspace --exclude zed-al
	@echo "--- 14/16 semantic-feature clippy + tests (the unsafe CLR FFI) ---"
	cargo clippy -p al-semantic -p al-lsp --features semantic --all-targets -- -D warnings
	cargo test -p al-semantic --features semantic
	@echo "--- 15/16 test + build the actual Zed extension ---"
	cargo test -p zed-al
	cargo build -p zed-al --target wasm32-wasip2 --release
	@bash scripts/check-zed-wasm-component.sh "$(WASM_BIN)"
	@echo "--- 16/16 local package-manifest audit for publishable crates ---"
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

# ── Claude Code plugin ───────────────────────────────────────────
# Manifest JSON validity, shellcheck on plugin/scripts/*.sh, and a name plus a
# description in the frontmatter of every skill and agent.
plugin-validate:
	@bash scripts/check-plugin.sh

# Ground-truth checks for the plugin's skills, against al-explorer directly:
# no LLM involved. Needs a built al-explorer on PATH or at
# target/release/al-explorer (build it first with `make rust` or `cargo build
# --release -p al-explorer`); a case reports itself skipped, not failed, when
# neither is found.
plugin-evals:
	@bash plugin/evals/run.sh

# ── Clean ────────────────────────────────────────────────────────
clean:
	cargo clean
	@# `dotnet clean` is best-effort: on a machine without a dotnet install,
	@# invoking it at all fails with exit 127 (stderr suppression alone does
	@# not change that), which would abort this target after `cargo clean`
	@# already ran. Skip it outright when there's no `dotnet` on PATH, and
	@# don't let a clean failure (bad project state, etc.) abort either.
	@if [ -f $(ALSEMANTIC_PROJ) ] && command -v dotnet >/dev/null 2>&1; then \
		dotnet clean $(ALSEMANTIC_PROJ) --nologo -v quiet 2>/dev/null || true; \
	fi
	@echo "Clean complete."
