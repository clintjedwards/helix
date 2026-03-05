SHELL        := /bin/bash
BIN_DIR      := $(HOME)/.bin
CONFIG_DIR   := $(HOME)/.config/helix
RUNTIME_LINK := $(CONFIG_DIR)/runtime

.PHONY: all build install fetch health

# Default: build and install
all: install

build:
	cargo build --release

# Full install: build, copy binary, wire up runtime, compile grammars
install: build
	@mkdir -p $(BIN_DIR)
	install -m755 target/release/hx $(BIN_DIR)/hx
	@mkdir -p $(CONFIG_DIR)
	@if [ -L $(RUNTIME_LINK) ]; then \
		echo "Updating runtime symlink -> $(CURDIR)/runtime"; \
		ln -sf $(CURDIR)/runtime $(RUNTIME_LINK); \
	elif [ -e $(RUNTIME_LINK) ]; then \
		echo "Warning: $(RUNTIME_LINK) already exists and is not a symlink."; \
		echo "         Remove it manually if you want to use this repo's runtime."; \
	else \
		echo "Creating runtime symlink -> $(CURDIR)/runtime"; \
		ln -sf $(CURDIR)/runtime $(RUNTIME_LINK); \
	fi
	$(BIN_DIR)/hx --grammar build
	@echo ""
	@echo "Installed: $(BIN_DIR)/hx"
	@echo "Runtime:   $(RUNTIME_LINK) -> $(CURDIR)/runtime"
	@echo ""
	@echo "Make sure $(BIN_DIR) is in your PATH."

# Fetch any new/missing grammar sources and recompile (run when grammars change)
fetch:
	$(BIN_DIR)/hx --grammar fetch
	$(BIN_DIR)/hx --grammar build

# Run the helix health check
health:
	$(BIN_DIR)/hx --health
