//! PML Language Server — Stage 2.
//!
//! Features:
//!   - Static builtins library loaded from `data/builtins.json` at startup
//!   - Tree-sitter parsing of every open document
//!   - Per-document symbol extraction (functions, methods, objects, globals)
//!   - Completion + hover that merges builtins with the current file's symbols

mod parser;
mod symbols;

use dashmap::DashMap;
use parser::PmlParser;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use symbols::{Symbol, SymbolKind, SymbolTable};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

// =============================================================================
// Builtin data model (unchanged from Stage 1)
// =============================================================================

#[derive(Clone, Debug, Deserialize)]
struct Builtin {
    name: String,
    kind: String,
    detail: String,
    documentation: String,
    #[serde(default)]
    insert_text: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // reserved for stage 3: context-aware filtering
    receiver_types: Vec<String>,
}

fn parse_kind(s: &str) -> CompletionItemKind {
    match s.to_ascii_lowercase().as_str() {
        "method" => CompletionItemKind::METHOD,
        "function" => CompletionItemKind::FUNCTION,
        "constructor" => CompletionItemKind::CONSTRUCTOR,
        "variable" => CompletionItemKind::VARIABLE,
        "keyword" => CompletionItemKind::KEYWORD,
        "snippet" => CompletionItemKind::SNIPPET,
        "field" | "member" => CompletionItemKind::FIELD,
        "class" | "object" => CompletionItemKind::CLASS,
        "enum" => CompletionItemKind::ENUM,
        "constant" => CompletionItemKind::CONSTANT,
        "operator" => CompletionItemKind::OPERATOR,
        _ => CompletionItemKind::TEXT,
    }
}

fn symbol_kind_to_completion(k: &SymbolKind) -> CompletionItemKind {
    match k {
        SymbolKind::Function => CompletionItemKind::FUNCTION,
        SymbolKind::Method => CompletionItemKind::METHOD,
        SymbolKind::Object => CompletionItemKind::CLASS,
        SymbolKind::Member => CompletionItemKind::FIELD,
        SymbolKind::Parameter => CompletionItemKind::VARIABLE,
        SymbolKind::Variable => CompletionItemKind::VARIABLE,
    }
}

fn find_builtins_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PML_LSP_BUILTINS") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join("../data/builtins.json"),
                dir.join("../../data/builtins.json"),
                dir.join("data/builtins.json"),
            ] {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    let cwd_relative = PathBuf::from("data/builtins.json");
    if cwd_relative.exists() {
        return Some(cwd_relative);
    }
    None
}

fn load_builtins() -> Vec<Builtin> {
    match find_builtins_path() {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Vec<Builtin>>(&contents) {
                Ok(list) => {
                    eprintln!(
                        "pml-lsp: loaded {} builtins from {}",
                        list.len(),
                        path.display()
                    );
                    list
                }
                Err(e) => {
                    eprintln!("pml-lsp: failed to parse {}: {}", path.display(), e);
                    Vec::new()
                }
            },
            Err(e) => {
                eprintln!("pml-lsp: failed to read {}: {}", path.display(), e);
                Vec::new()
            }
        },
        None => {
            eprintln!("pml-lsp: builtins.json not found; serving empty builtin list");
            Vec::new()
        }
    }
}

// =============================================================================
// LSP backend
// =============================================================================

struct Backend {
    client: Client,
    documents: DashMap<Url, String>,
    /// Per-document symbol tables. Updated on didOpen/didChange.
    symbol_tables: DashMap<Url, SymbolTable>,
    /// Loaded once at startup.
    builtins: Arc<Vec<Builtin>>,
    /// Tree-sitter parser. Wrapped in Arc because handlers are async.
    parser: Arc<PmlParser>,
}

impl Backend {
    /// Re-parse a document and refresh its symbol table.
    /// Called from didOpen and didChange.
    fn reparse(&self, uri: &Url, text: &str) {
        if let Some(tree) = self.parser.parse(text) {
            let table = SymbolTable::extract(&tree, text);
            self.symbol_tables.insert(uri.clone(), table);
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), "!".to_string()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "pml-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "pml-lsp v{} initialized with {} builtins (tree-sitter ON)",
                    env!("CARGO_PKG_VERSION"),
                    self.builtins.len()
                ),
            )
            .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text;
        self.reparse(&uri, &text);
        self.documents.insert(uri, text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().next() {
            let uri = params.text_document.uri.clone();
            self.reparse(&uri, &change.text);
            self.documents.insert(uri, change.text);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri);
        self.symbol_tables.remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;

        // Start with the static builtins
        let mut items: Vec<CompletionItem> = self
            .builtins
            .iter()
            .map(|b| {
                let mut item = CompletionItem {
                    label: b.name.clone(),
                    kind: Some(parse_kind(&b.kind)),
                    detail: Some(b.detail.clone()),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: b.documentation.clone(),
                    })),
                    ..Default::default()
                };
                if let Some(snippet) = &b.insert_text {
                    item.insert_text = Some(snippet.clone());
                    item.insert_text_format = Some(InsertTextFormat::SNIPPET);
                }
                item
            })
            .collect();

        // Add user-defined symbols from the current document
        if let Some(table) = self.symbol_tables.get(uri) {
            for sym in &table.symbols {
                items.push(CompletionItem {
                    label: sym.name.clone(),
                    kind: Some(symbol_kind_to_completion(&sym.kind)),
                    detail: Some(sym.detail.clone()),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: sym.documentation.clone(),
                    })),
                    ..Default::default()
                });
            }
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let Some(text) = self.documents.get(&uri) else {
            return Ok(None);
        };
        let Some(word) = word_at_position(&text, position) else {
            return Ok(None);
        };

        // 1) Check this file's user-defined symbols first — most specific wins
        if let Some(table) = self.symbol_tables.get(&uri) {
            for sym in &table.symbols {
                if sym.name.eq_ignore_ascii_case(&word) {
                    return Ok(Some(make_hover(&sym.detail, &sym.documentation)));
                }
            }
        }

        // 2) Fall back to builtins
        for b in self.builtins.iter() {
            let ident = b.name.split('(').next().unwrap_or(&b.name);
            if ident.eq_ignore_ascii_case(&word) {
                return Ok(Some(make_hover(&b.detail, &b.documentation)));
            }
        }

        Ok(None)
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

fn make_hover(detail: &str, documentation: &str) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```pml\n{}\n```\n\n{}", detail, documentation),
        }),
        range: None,
    }
}

fn word_at_position(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    let chars: Vec<char> = line.chars().collect();
    if col > chars.len() {
        return None;
    }

    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '!';

    let mut start = col;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }

    if start == end {
        None
    } else {
        Some(chars[start..end].iter().collect())
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let builtins = Arc::new(load_builtins());

    let (service, socket) = LspService::new(|client| Backend {
        client,
        documents: DashMap::new(),
        symbol_tables: DashMap::new(),
        builtins: builtins.clone(),
        parser: Arc::new(PmlParser::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
