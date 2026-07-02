# Zed AL Extension — Build System
#
# Usage:
#   make install   — first-time setup: build everything + symlink into PATH + Zed
#   make build     — rebuild everything (all Rust crates + WASM extension + .NET bridges)
#   make rust      — rebuild all Rust crates (native, excludes zed-al)
#   make wasm      — rebuild only the WASM extension (zed-al, wasm32-wasip1)
#   make bridges   — rebuild just .NET bridges
#   make grammar   — regenerate the tree-sitter-al parser sources (required before
#                    `tree-sitter build` from a fresh clone — see F-006)
#   make language  — regenerate only the Zed-facing languages/al package files
#   make repro-artifacts — regenerate generated artifacts; fail on any diff (CI drift guard)
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
WASM_BIN := $(ROOT)/target/wasm32-wasip1/release/zed_al.wasm

.PHONY: build install install-lsp dev-setup watch rust wasm bridges grammar language repro-artifacts release-dryrun clean

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
	@# (audit 2026-06-12). Use `al-explorer` in scripts; `al` is best-effort.
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
# Rebuild al-lsp WITH --features semantic and COPY it into INSTALL_DIR.
#
# WHY a copy and not a symlink: ~/.local/bin/al-lsp must be the
# `--features semantic` binary (the real in-process .NET CLR host). A symlink
# into target/debug/al-lsp is unsafe because any `cargo build --workspace`
# (tests, coverage runs, plain builds) compiles al-lsp WITHOUT the feature and
# rewrites that same path with the no-op stub host. cargo does not encode
# features in the bin path, so last-build-wins and the stub silently replaces
# the real host — Zed then shows "AL semantic bridge failed to initialize:
# Bridge not initialized" on every semantic request. Copying decouples the
# installed binary from cargo's shared output path. The bridge dir is copied
# alongside so it also resolves via host.rs Strategy 2 (<exe-dir>/bridge/) even
# after `cargo clean`. Run this after editing al-lsp (or any crate it depends
# on) to refresh Zed's binary.
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
	@rustup target add wasm32-wasip1 2>/dev/null || true
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
	cargo build -p zed-al --target wasm32-wasip1 --release
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

# ── Release readiness dry-run (READ-ONLY, never publishes) ────────
# Runs the gates a release would, without uploading anything: repo-slug
# consistency, version/metadata/grammar-rev alignment, generated-artifact
# reproducibility, a full native build (incl. the real --features semantic
# al-lsp), the test suite, and `cargo publish --dry-run` for every publishable
# crate. zed-al is excluded from build/test (it is a wasm32-wasip1 cdylib; use
# `make wasm`). See Docs/testing-guide.md §6 for the publish-dry-run caveat:
# until the first real publish, crates with not-yet-published path-deps report
# "blocked on unpublished workspace dep (expected)" and do NOT fail the run.
release-dryrun:
	@echo "=== Release dry-run (read-only; nothing is published) ==="
	@echo "--- 1/6 repo-slug consistency ---"
	@bash scripts/check-repo-consistency.sh
	@echo "--- 2/6 release hygiene (versions / submodule / grammar rev / generated assets) ---"
	@bash scripts/check-release-hygiene.sh
	@echo "--- 3/6 reproducible generated artifacts ---"
	@$(MAKE) --no-print-directory repro-artifacts
	@echo "--- 4/6 build workspace (excl zed-al) + real semantic al-lsp ---"
	cargo build --workspace --exclude zed-al
	cargo build -p al-lsp --bin al-lsp --features semantic
	@echo "--- 5/6 test workspace (excl zed-al) ---"
	cargo test --workspace --exclude zed-al
	@echo "--- 6/6 cargo publish --dry-run for publishable crates ---"
	@fail=0; \
	for dir in crates/*/; do \
		f="$$dir/Cargo.toml"; \
		[ -f "$$f" ] || continue; \
		name=$$(awk -F'"' '/^name[[:space:]]*=/{print $$2; exit}' "$$f"); \
		case " $(PUBLISH_EXCLUDE) " in *" $$name "*) continue;; esac; \
		grep -Eq '^publish[[:space:]]*=[[:space:]]*false' "$$f" && continue; \
		out=$$(cargo publish --dry-run --no-verify -p "$$name" 2>&1); \
		if [ $$? -eq 0 ]; then \
			echo "  $$name: OK (dry-run)"; \
		elif echo "$$out" | grep -q "no matching package named"; then \
			echo "  $$name: blocked on unpublished workspace dep (expected pre-first-publish)"; \
		elif echo "$$out" | grep -q "failed to select a version for the requirement"; then \
			echo "  $$name: blocked on unpublished/unmatched workspace dep version (expected pre-first-publish)"; \
		else \
			echo "  $$name: FAILED"; echo "$$out" | tail -8; fail=1; \
		fi; \
	done; \
	[ $$fail -eq 0 ] || { echo "release-dryrun: a crate failed publish dry-run for a non-dependency reason"; exit 1; }
	@echo ""
	@echo "Release dry-run complete (read-only — nothing published)."

# ── Clean ────────────────────────────────────────────────────────
clean:
	cargo clean
	@if [ -f $(ALSEMANTIC_PROJ) ]; then dotnet clean $(ALSEMANTIC_PROJ) --nologo -v quiet 2>/dev/null; fi
	@echo "Clean complete."
