#!/usr/bin/env bash
set -euo pipefail

NVIM_DATA="${XDG_DATA_HOME:-$HOME/.local/share}/nvim"
NVIM_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/nvim"

echo "Building LSP..."
cargo build --release

echo "Setting up grammar..."
GRAMMAR_DIR="$NVIM_CONFIG/lua/user/tree-sitter-pml"
if [ ! -d "$GRAMMAR_DIR" ]; then
  git clone https://github.com/djee79/tree-sitter-pml.git "$GRAMMAR_DIR"
fi
cd "$GRAMMAR_DIR"
npx tree-sitter generate
npx tree-sitter build -o pml.so

mkdir -p "$NVIM_DATA/site/parser"
ln -sf "$GRAMMAR_DIR/pml.so" "$NVIM_DATA/site/parser/pml.so"

mkdir -p "$NVIM_CONFIG/queries"
ln -sf "$GRAMMAR_DIR/queries/pml" "$NVIM_CONFIG/queries/pml"

echo "Done. Binary at: $(pwd)/target/release/pml-lsp"
echo "Update your LazyVim pml-lsp.lua plugin spec to point at that path."
