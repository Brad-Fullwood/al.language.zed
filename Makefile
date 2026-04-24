# Zed AL Extension — Build System
#
# Usage:
#   make install   — first-time setup: build everything + symlink into PATH + Zed
#   make build     — rebuild everything (all Rust crates + WASM extension + .NET bridges)
#   make rust      — rebuild all Rust crates (native, excludes zed-al)
#   make wasm      — rebuild only the WASM extension (zed-al, wasm32-wasip1)
#   make bridges   — rebuild just .NET bridges
#   make clean     — clean all build artifacts

SHELL := /bin/bash
ROOT := $(shell pwd)
LSP_BIN := $(ROOT)/target/debug/al-lsp
CLI_BIN := $(ROOT)/target/debug/al
EXPLORER_BIN := $(ROOT)/target/debug/al-explorer
INSTALL_DIR := $(HOME)/.local/bin
ZED_EXT_DIR := $(HOME)/.local/share/zed/extensions/installed

# .NET bridge projects (quoted for paths with spaces)
ALSEMANTIC_PROJ := "$(ROOT)/crates/al-semantic/bridge/AlBridge.csproj"
WASM_BIN := $(ROOT)/target/wasm32-wasip1/release/zed_al.wasm

.PHONY: build install rust wasm bridges clean

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
	@if [ ! -L "$(INSTALL_DIR)/al" ] && [ ! -f "$(INSTALL_DIR)/al" ]; then \
		ln -sf "$(CLI_BIN)" "$(INSTALL_DIR)/al"; \
		echo "Symlinked al -> $(INSTALL_DIR)/al"; \
	else \
		echo "al already in $(INSTALL_DIR) (OK)"; \
	fi
	@if [ ! -L "$(INSTALL_DIR)/al-explorer" ] && [ ! -f "$(INSTALL_DIR)/al-explorer" ]; then \
		ln -sf "$(EXPLORER_BIN)" "$(INSTALL_DIR)/al-explorer"; \
		echo "Symlinked al-explorer -> $(INSTALL_DIR)/al-explorer"; \
	else \
		echo "al-explorer already in $(INSTALL_DIR) (OK)"; \
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

# ── Build ALL Rust workspace crates ──────────────────────────────
rust:
	@echo "=== Building all Rust crates ==="
	cargo build --workspace --exclude zed-al

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

# ── Clean ────────────────────────────────────────────────────────
clean:
	cargo clean
	@if [ -f $(ALSEMANTIC_PROJ) ]; then dotnet clean $(ALSEMANTIC_PROJ) --nologo -v quiet 2>/dev/null; fi
	@echo "Clean complete."
