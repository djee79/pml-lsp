# pml-lsp — PML Language Server

A Language Server Protocol implementation for AVEVA PML, providing
IntelliSense-style completion and hover for PML built-ins.

## Project layout

```
pml-lsp/
├── Cargo.toml
├── src/
│   └── main.rs            # LSP server (loads builtins.json at startup)
└── data/
    └── builtins.json      # Built-in symbol definitions
```

## Building and using

```bash
cargo build --release
```

The binary lands at `target/release/pml-lsp`. Point your editor's LSP
config at it. Restart the server after rebuilding or after editing
`data/builtins.json`.

The server looks for `builtins.json` in this order:
1. `$PML_LSP_BUILTINS` environment variable (explicit override)
2. Relative to the binary at `../data/builtins.json` or `../../data/builtins.json`
3. `./data/builtins.json` (when run from the project root)

If you move the binary elsewhere, set `$PML_LSP_BUILTINS` to its absolute path.

## Adding a new built-in

Each entry in `data/builtins.json` is an object with these fields:

| Field            | Required | Notes                                                              |
| ---------------- | -------- | ------------------------------------------------------------------ |
| `name`           | yes      | What appears in the completion menu and matches against typed text |
| `kind`           | yes      | `"Method"`, `"Function"`, `"Constructor"`, `"Variable"`, `"Keyword"`, `"Snippet"`, `"Field"`, `"Class"`, `"Constant"`, `"Operator"` |
| `detail`         | yes      | Short signature shown in the menu (e.g., `"Length() IS REAL"`)     |
| `documentation`  | yes      | Markdown shown in the side panel and on hover                      |
| `insert_text`    | no       | LSP snippet body (e.g., `"DO !${1:i} FROM 1 TO ${2:10}\n    $0\nENDDO"`) |
| `receiver_types` | no       | Array of object types this method belongs to, e.g. `["STRING"]`. Reserved for context-aware filtering in a future stage. |

### Conventions

- **Consolidate overloads.** When the manual lists `Replace(STRING, STRING)`,
  `Replace(STRING, STRING, REAL)`, and `Replace(STRING, STRING, REAL, REAL)`,
  create *one* entry called `Replace` and list all overloads in the
  `documentation` markdown. Otherwise the menu shows three identical-looking
  items.
- **Paraphrase the manual.** Don't copy descriptions verbatim — write them
  in your own words. The result is usually clearer than the original.
- **Detail field is signature-only.** Keep it short — one line if possible.
  Use `[ ]` for optional arguments: `Trim([STRING options[, STRING char]]) IS STRING`.

## Workflow for a new object section

1. Pick an object from the AVEVA reference manual (e.g., `REAL`, `ARRAY`, `DBREF`).
2. Read its methods table from the PDF.
3. Append entries to `data/builtins.json` following the conventions above.
4. Validate the JSON: `python3 -c "import json; json.load(open('data/builtins.json'))"`
5. Restart your editor's LSP. No rebuild required for JSON-only changes.

## Stages

- **Stage 1 (current):** Static built-ins from JSON. Completion + hover work.
- **Stage 2:** Tree-sitter integration to extract user-defined functions,
  methods, objects, and variables from the open document. Adds context
  awareness.
- **Stage 3:** Cross-file resolution by walking project directories at startup.
- **Stage 4:** Signature help, diagnostics, go-to-definition.
