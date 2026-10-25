# pml-lsp

A language server for AVEVA PML, built on the
[tree-sitter-pml](https://github.com/djee79/tree-sitter-pml) grammar.

## Features

- **Workspace index**: every PML file under the project root (or the open
  file's folder when there is no project) is parsed at startup and kept fresh
  through file watchers. Symbols resolve across files: a method defined in
  `box.pmlobj` completes and jumps from `main.pmlfrm`.
- **Diagnostics**: syntax errors from the parse tree, confined to the line
  they occur on.
- **Completion**, aware of what you are typing:
  - after `.`: the receiver is resolved when possible and only its members
    are offered: `!this.` gives the current object's or form's methods,
    members and gadgets; `!b.` after `!b = object BOX()` or a `!b is BOX`
    parameter gives BOX's methods from any file; `!!myForm.input.` gives
    TEXT gadget methods; `!s.` after `!s = 'x'` gives STRING methods.
    Types flow through chains and return types: after
    `!s = !this.name.val.upcase()`, `!s.` knows it is a STRING (`.val` of a
    TEXT gadget is taken as STRING, TOGGLE as BOOLEAN, OPTION/LIST as REAL). Hovering a
    variable shows its inferred type. When the type is unknown, everything
    that fits after a dot is offered;
  - after `!` / `!!`: every variable used in the file, plus user functions,
    forms and globals;
  - otherwise: keywords, snippets, constructors, PML functions and user
    objects/functions/forms.
- **Hover** on builtins and user symbols, showing the signature and the
  leading `--` comment block as documentation.
- **Go to definition** for functions, methods, objects, members, forms,
  gadgets and globals, in the current file or any indexed file.
- **Signature help** while typing call arguments, for user functions and
  methods, object constructors and builtins. Follows `$` continuations.
- **Semantic tokens**: `!!name` is coloured as a function, form or global
  according to what defines it; `.name` as a user method, builtin method,
  gadget or member; type names as user objects or builtin types. Layered on
  top of tree-sitter highlighting through Neovim's `@lsp.type.*` groups.
- **Formatting**: re-indents blocks (`define`, `if`, `do`, `handle`,
  `setup form`, `frame`) from the syntax tree and strips trailing whitespace,
  leaving everything else untouched. Continuation lines after `$` are
  indented two levels. Works on the whole file or a range.
- **Code actions**: create a stub for an undefined method called on the
  current line (parameters named after `!var` arguments); add a
  `handle any … endhandle` block after a statement; fix indentation.
- **Find references** and **rename** across all indexed files. Matching is
  case-insensitive and namespace-aware: renaming `.scale` touches methods and
  calls, not a `!scale` variable. Local `!x` variables are renamed inside
  their function or method only.
- **Document symbols** (outline): objects with their members, forms with
  their gadgets, functions, methods, globals. **Workspace symbols**
  (`<leader>sS`) search every indexed file.

Builtins come from `data/builtins.json`, which is embedded in the binary at
build time. Set `$PML_LSP_BUILTINS` to a JSON file to override it at runtime
without rebuilding.

## Building

```sh
cargo build --release          # binary: target/release/pml-lsp
cargo test
```

The grammar is fetched from GitHub. To build against a local checkout of
`tree-sitter-pml`, uncomment the `[patch]` section in `Cargo.toml`.

`./install.sh --grammar ../tree-sitter-pml` builds both the server and the
parser shared library and links the parser and queries into Neovim.

## Neovim (LazyVim)

```lua
-- lua/plugins/pml-lsp.lua
return {
  {
    "neovim/nvim-lspconfig",
    opts = {
      servers = {
        pml_ls = {
          cmd = { vim.fn.expand("~/dev/pml-lsp/target/release/pml-lsp") },
          filetypes = { "pml" },
          root_markers = { ".git" },
        },
      },
    },
  },
}
```

The `pml` filetype and the tree-sitter parser are set up by the grammar repo;
see its README.

Useful commands: `gd` go to definition, `gr` references, `<leader>cr`
rename, `<leader>ca` code actions, `<leader>cf` format, `K` hover, `<C-k>`
signature help in insert mode, `<leader>ss` document symbols, `<leader>sS`
workspace symbols, `:LspInfo`, `:LspLog`.

LazyVim formats on save by default when the server supports it. Disable
that for PML with `vim.g.autoformat = false` or per filetype with
`vim.b.autoformat = false` in `after/ftplugin/pml.lua` if you prefer
formatting on demand only.

The project root is the folder containing `.git`. Without one, the server
indexes the folder of each file you open.

## Project layout

```
src/main.rs          LSP server: capabilities, completion, hover, definition, symbols
src/parser.rs        tree-sitter parser wrapper
src/symbols.rs       symbol extraction from the parse tree
src/diagnostics.rs   ERROR / MISSING nodes -> diagnostics
src/context.rs       cursor context (after `.`, after `!`, statement), word lookup, variable type inference
src/workspace.rs     project-wide file discovery and indexing
src/signature.rs     call-site detection and signature parsing
src/references.rs    namespace-aware occurrences for references and rename
src/types.rs         static type inference over expression chains
src/semantic.rs      semantic tokens
src/format.rs        indentation from the syntax tree
src/actions.rs       code actions
data/builtins.json   builtin methods, functions, keywords, snippets
```

## Adding a builtin

Each entry in `data/builtins.json`:

| Field            | Required | Notes |
| ---------------- | -------- | ----- |
| `name`           | yes      | Completion label; matched case-insensitively on hover |
| `kind`           | yes      | `Method`, `Function`, `Constructor`, `Variable`, `Keyword`, `Snippet`, `Field`, `Class`, `Constant`, `Operator` |
| `detail`         | yes      | One-line signature, e.g. `Length() IS REAL` |
| `documentation`  | yes      | Markdown for the side panel and hover |
| `insert_text`    | no       | LSP snippet body |
| `receiver_types` | no       | Object types a method belongs to, e.g. `["STRING"]`. Methods are only offered after `.`, and filtered by this when the receiver type is known |

Conventions: consolidate overloads into one entry and list them in the
documentation; keep `detail` to one line; paraphrase the manual.

Rebuild after editing the JSON (it is embedded), or point
`$PML_LSP_BUILTINS` at the file for live edits.

## Roadmap

- Keyword case normalisation as an optional formatting pass.
- Unused-variable and unknown-method diagnostics.
