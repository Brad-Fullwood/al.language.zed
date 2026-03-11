# Zed AL Extension — Build System
#
# Usage:
#   make install   — first-time setup: build everything + symlink into PATH + Zed
#   make build     — rebuild everything (all Rust crates + .NET bridges)
#   make rust      — rebuild all Rust crates
#   make bridges   — rebuild just .NET bridges
#   make clean     — clean all build artifacts

SHELL := /bin/bash
ROOT := $(shell pwd)
LSP_BIN := $(ROOT)/target/debug/al-lsp
CLI_BIN := $(ROOT)/target/debug/al-cli
INSTALL_DIR := $(HOME)/.local/bin
ZED_EXT_DIR := $(HOME)/.local/share/zed/extensions/installed

# .NET bridge projects (quoted for paths with spaces)
ALDAP_PROJ := "$(ROOT)/crates/al-dap/dotnet/AlDap/AlDap.csproj"
ALSEMANTIC_PROJ := "$(ROOT)/crates/al-semantic/dotnet/AlSemantic/AlSemantic.csproj"

.PHONY: build install rust bridges clean

# ── Default: rebuild everything ──────────────────────────────────
build: rust bridges
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
	@if [ ! -L "$(INSTALL_DIR)/al-cli" ] && [ ! -f "$(INSTALL_DIR)/al-cli" ]; then \
		ln -sf "$(CLI_BIN)" "$(INSTALL_DIR)/al-cli"; \
		echo "Symlinked al-cli -> $(INSTALL_DIR)/al-cli"; \
	else \
		echo "al-cli already in $(INSTALL_DIR) (OK)"; \
	fi
	@mkdir -p "$(ZED_EXT_DIR)"
	@if [ ! -L "$(ZED_EXT_DIR)/al" ] && [ ! -d "$(ZED_EXT_DIR)/al" ]; then \
		ln -sf "$(ROOT)" "$(ZED_EXT_DIR)/al"; \
		echo "Dev extension symlinked into Zed."; \
		echo "NOTE: Run 'zed: install dev extension' once from the command palette to build extension.wasm."; \
	else \
		echo "Dev extension already installed in Zed (OK)"; \
	fi
	@echo ""
	@echo "Install complete."

# ── Build ALL Rust workspace crates ──────────────────────────────
rust:
	@echo "=== Building all Rust crates ==="
	cargo build --workspace --exclude zed-al

# ── Build .NET bridges ───────────────────────────────────────────
bridges:
	@echo "=== Building .NET bridges ==="
	@if [ -f $(ALDAP_PROJ) ]; then \
		dotnet build $(ALDAP_PROJ) --nologo -v quiet && echo "  AlDap: OK"; \
	fi
	@if [ -f $(ALSEMANTIC_PROJ) ]; then \
		dotnet build $(ALSEMANTIC_PROJ) --nologo -v quiet && echo "  AlSemantic: OK"; \
	fi

# ── Clean ────────────────────────────────────────────────────────
clean:
	cargo clean
	@if [ -f $(ALDAP_PROJ) ]; then dotnet clean $(ALDAP_PROJ) --nologo -v quiet 2>/dev/null; fi
	@if [ -f $(ALSEMANTIC_PROJ) ]; then dotnet clean $(ALSEMANTIC_PROJ) --nologo -v quiet 2>/dev/null; fi
	@echo "Clean complete."
