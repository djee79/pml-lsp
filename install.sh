#!/usr/bin/env bash
# Build pml-lsp and (optionally) install the tree-sitter-pml grammar for Neovim.
#
#   ./install.sh                 build the LSP only
#   ./install.sh --grammar DIR   also build DIR/pml.so and link parser + queries
#                                into Neovim (DIR is a tree-sitter-pml checkout)
set -euo pipefail

NVIM_DATA="${XDG_DATA_HOME:-$HOME/.local/share}/nvim"
NVIM_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/nvim"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building pml-lsp (release)..."
cargo build --release --manifest-path "$HERE/Cargo.toml"
BIN="$HERE/target/release/pml-lsp"
echo "Binary: $BIN"

if [ "${1:-}" = "--grammar" ]; then
  GRAMMAR_DIR="${2:?usage: install.sh --grammar /path/to/tree-sitter-pml}"
  echo "Building grammar in $GRAMMAR_DIR..."
  (cd "$GRAMMAR_DIR" && tree-sitter build -o pml.so)

  mkdir -p "$NVIM_DATA/site/parser" "$NVIM_CONFIG/queries/pml"
  ln -sfn "$GRAMMAR_DIR/pml.so" "$NVIM_DATA/site/parser/pml.so"
  for q in highlights locals folds injections; do
    ln -sfn "$GRAMMAR_DIR/queries/$q.scm" "$NVIM_CONFIG/queries/pml/$q.scm"
  done
  echo "Linked parser and queries into Neovim."
fi

cat <<EOF

Neovim (LazyVim) config, e.g. lua/plugins/pml-lsp.lua:

  return {
    { "neovim/nvim-lspconfig",
      opts = { servers = { pml_ls = {
        cmd = { "$BIN" },
        filetypes = { "pml" },
        root_markers = { ".git" },
      } } } },
  }
EOF
