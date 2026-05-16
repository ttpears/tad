PREFIX ?= $(HOME)/.local
BIN_DIR := $(PREFIX)/bin
BASH_COMP_DIR := $(PREFIX)/share/bash-completion/completions
ZSH_COMP_DIR := $(PREFIX)/share/zsh/site-functions

.PHONY: build install uninstall clean test

build:
	cargo build --release

install: build
	install -Dm755 target/release/tad $(BIN_DIR)/tad
	install -Dm644 completions/tad.bash $(BASH_COMP_DIR)/tad
	install -Dm644 completions/_tad $(ZSH_COMP_DIR)/_tad
	@echo
	@echo "Installed:"
	@echo "  $(BIN_DIR)/tad"
	@echo "  $(BASH_COMP_DIR)/tad"
	@echo "  $(ZSH_COMP_DIR)/_tad"
	@echo
	@echo "Make sure $(BIN_DIR) is in your PATH."
	@echo "For zsh, ensure $(PREFIX)/share/zsh/site-functions is in your fpath."

uninstall:
	rm -f $(BIN_DIR)/tad
	rm -f $(BASH_COMP_DIR)/tad
	rm -f $(ZSH_COMP_DIR)/_tad

clean:
	cargo clean

test:
	cargo test
