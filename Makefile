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
#   make clean     — clean all build artifacts

SHELL := /bin/bash
ROOT := $(shell pwd)
LSP_BIN := $(ROOT)/target/debug/al-lsp
EXPLORER_BIN := $(ROOT)/target/debug/al-explorer
INSTALL_DIR := $(HOME)/.local/bin
ZED_EXT_DIR := $(HOME)/.local/share/zed/extensions/installed

# .NET bridge projects (quoted for paths with spaces)
ALSEMANTIC_PROJ := "$(ROOT)/crates/al-core/bridge/AlBridge.csproj"
WASM_BIN := $(ROOT)/target/wasm32-wasip1/release/zed_al.wasm

.PHONY: build install dev-setup watch rust wasm bridges grammar clean

# ── Default: rebuild everything ──────────────────────────────────
build: rust wasm bridges
	@echo ""
	@echo "Build complete. Restart the LSP in Zed to pick up changes."

# ── First-time install ───────────────────────────────────────────
install: build
	@mkdir -p $(INSTALL_DIR)
	@if [ ! -L "$(INSTALL_DIR)/al-lsp" ] && [ ! -f "$(INSTALL_DIR)/al-lsp" ]; then \
		ln -sf "$(LSP_BIN)" "$(INSTALL_DIR)/al-lsp"; \
		echo "Symlinked al-lsp -> $(INSTALL_DIR)/al-lsp"; \
	else \
		echo "al-lsp already in $(INSTALL_DIR) (OK)"; \
	fi
	@if [ ! -L "$(INSTALL_DIR)/al-explorer" ] && [ ! -f "$(INSTALL_DIR)/al-explorer" ]; then \
		ln -sf "$(EXPLORER_BIN)" "$(INSTALL_DIR)/al-explorer"; \
		echo "Symlinked al-explorer -> $(INSTALL_DIR)/al-explorer"; \
	else \
		echo "al-explorer already in $(INSTALL_DIR) (OK)"; \
	fi
	@# `al` is a back-compat alias for al-explorer's CLI mode (the old al-cli binary)
	@if [ ! -L "$(INSTALL_DIR)/al" ] && [ ! -f "$(INSTALL_DIR)/al" ]; then \
		ln -sf "$(EXPLORER_BIN)" "$(INSTALL_DIR)/al"; \
		echo "Symlinked al -> $(INSTALL_DIR)/al (alias for al-explorer)"; \
	else \
		echo "al already in $(INSTALL_DIR) (OK)"; \
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
	cargo build -p al-core --bin al-lsp --features semantic

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

# ── Clean ────────────────────────────────────────────────────────
clean:
	cargo clean
	@if [ -f $(ALSEMANTIC_PROJ) ]; then dotnet clean $(ALSEMANTIC_PROJ) --nologo -v quiet 2>/dev/null; fi
	@echo "Clean complete."
