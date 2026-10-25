<#
.SYNOPSIS
  Build pml-lsp and (optionally) install the tree-sitter-pml grammar for Neovim on Windows.

.DESCRIPTION
  Windows counterpart of install.sh. Builds the release binary and, with
  -Grammar, builds pml.dll in the grammar checkout and copies the parser and
  the queries into Neovim's data and config folders (copies, not symlinks:
  symlinks need administrator rights on Windows). Re-run after every
  `git pull` in either repository.

.PARAMETER Grammar
  Path to a tree-sitter-pml checkout. Needs the tree-sitter CLI
  (npm install -g tree-sitter-cli@0.25.10) and a C compiler (Visual Studio
  Build Tools with the C++ workload, or zig with $env:CC = "zig cc").

.EXAMPLE
  .\install.ps1
  .\install.ps1 -Grammar ..\tree-sitter-pml
#>
[CmdletBinding()]
param(
  [string]$Grammar
)

$ErrorActionPreference = "Stop"
$Here = Split-Path -Parent $MyInvocation.MyCommand.Path

function Require-Command([string]$Name, [string]$Hint) {
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "'$Name' not found on PATH. $Hint"
  }
}

Require-Command cargo "Install Rust from https://rustup.rs (needs the Visual Studio Build Tools C++ workload)."

Write-Host "Building pml-lsp (release)..."
cargo build --release --manifest-path (Join-Path $Here "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$Bin = Join-Path $Here "target\release\pml-lsp.exe"
Write-Host "Binary: $Bin"

if ($Grammar) {
  $GrammarDir = (Resolve-Path $Grammar).Path
  Require-Command tree-sitter "Install it with: npm install -g tree-sitter-cli@0.25.10"

  Write-Host "Building grammar in $GrammarDir..."
  Push-Location $GrammarDir
  try {
    tree-sitter build -o pml.dll
    if ($LASTEXITCODE -ne 0) { throw "tree-sitter build failed" }
  } finally {
    Pop-Location
  }

  $NvimData = Join-Path $env:LOCALAPPDATA "nvim-data"
  $NvimConfig = Join-Path $env:LOCALAPPDATA "nvim"
  $ParserDir = Join-Path $NvimData "site\parser"
  $QueryDir = Join-Path $NvimConfig "queries\pml"
  New-Item -ItemType Directory -Force -Path $ParserDir, $QueryDir | Out-Null

  Copy-Item -Force (Join-Path $GrammarDir "pml.dll") (Join-Path $ParserDir "pml.dll")
  foreach ($q in "highlights", "locals", "folds", "injections") {
    Copy-Item -Force (Join-Path $GrammarDir "queries\$q.scm") (Join-Path $QueryDir "$q.scm")
  }
  Write-Host "Copied parser to $ParserDir and queries to $QueryDir."
  Write-Host "Restart Neovim to load the new parser (the .dll is locked while it runs)."
}

# Lua needs forward slashes or escaped backslashes.
$BinLua = $Bin -replace "\\", "/"
Write-Host @"

Neovim (LazyVim) config, e.g. lua/plugins/pml-lsp.lua:

  return {
    { "neovim/nvim-lspconfig",
      opts = { servers = { pml_ls = {
        cmd = { "$BinLua" },
        filetypes = { "pml" },
        root_markers = { ".git" },
      } } } },
  }
"@
