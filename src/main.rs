//! PML Language Server.
//!
//! Features:
//!   - Builtins library (`data/builtins.json`, embedded in the binary and
//!     overridable with `$PML_LSP_BUILTINS`)
//!   - Tree-sitter parsing of every open document (`tree-sitter-pml`)
//!   - Syntax diagnostics from the parse tree
//!   - Workspace index of every PML file under the project root, kept fresh
//!     through file watchers, so symbols resolve across files
//!   - Document symbols and workspace symbol search
//!   - Context-aware completion (after `.`, after `!`/`!!`, statement start)
//!     with receiver-type resolution for `!this.`, `!!form.gadget.`, typed
//!     parameters and `!x = object TYPE()` assignments
//!   - Hover and go-to-definition for builtins and user symbols
//!   - Signature help, find-references and rename
//!   - Semantic tokens for functions, forms, globals, methods, gadgets, types
//!   - Formatting (indentation) and code actions (method stub, handle block)

mod actions;
mod context;
mod diagnostics;
mod format;
mod parser;
mod references;
mod semantic;
mod signature;
mod symbols;
mod types;
mod workspace;

use context::{Context, Decl};
use dashmap::DashMap;
use parser::PmlParser;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use symbols::{Symbol, SymbolKind, SymbolTable};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

/// Fallback copy of the builtins, so the binary works without the data file.
const EMBEDDED_BUILTINS: &str = include_str!("../data/builtins.json");

// =============================================================================
// Builtins
// =============================================================================

#[derive(Clone, Debug, Deserialize)]
struct Builtin {
    name: String,
    kind: String,
    detail: String,
    documentation: String,
    #[serde(default)]
    insert_text: Option<String>,
    /// Object types this method belongs to, e.g. `["STRING"]`. Empty for
    /// functions, keywords and snippets.
    #[serde(default)]
    receiver_types: Vec<String>,
}

impl Builtin {
    fn completion_kind(&self) -> CompletionItemKind {
        match self.kind.to_ascii_lowercase().as_str() {
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

    /// Methods and fields are only offered after a `.`.
    fn is_member(&self) -> bool {
        matches!(self.kind.to_ascii_lowercase().as_str(), "method" | "field" | "member")
    }

    fn applies_to(&self, receiver_type: Option<&str>) -> bool {
        match receiver_type {
            None => true,
            Some(t) => {
                self.receiver_types.is_empty()
                    || self.receiver_types.iter().any(|r| r.eq_ignore_ascii_case(t))
            }
        }
    }

    fn to_completion(&self) -> CompletionItem {
        let mut item = CompletionItem {
            label: self.name.clone(),
            kind: Some(self.completion_kind()),
            detail: Some(self.detail.clone()),
            documentation: Some(markdown(&self.documentation)),
            sort_text: Some(format!("2{}", self.name.to_ascii_lowercase())),
            ..Default::default()
        };
        if let Some(snippet) = &self.insert_text {
            item.insert_text = Some(snippet.clone());
            item.insert_text_format = Some(InsertTextFormat::SNIPPET);
        }
        item
    }
}

fn markdown(value: &str) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: value.to_string(),
    })
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
    None
}

fn load_builtins() -> Vec<Builtin> {
    if let Some(path) = find_builtins_path() {
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Vec<Builtin>>(&s).map_err(|e| e.to_string()))
        {
            Ok(list) => {
                eprintln!("pml-lsp: loaded {} builtins from {}", list.len(), path.display());
                return list;
            }
            Err(e) => eprintln!(
                "pml-lsp: could not use {}: {}; falling back to embedded builtins",
                path.display(),
                e
            ),
        }
    }
    let list: Vec<Builtin> = serde_json::from_str(EMBEDDED_BUILTINS).unwrap_or_default();
    eprintln!("pml-lsp: using {} embedded builtins", list.len());
    list
}

// =============================================================================
// Documents and workspace
// =============================================================================

#[derive(Clone)]
struct Document {
    text: String,
    symbols: Arc<SymbolTable>,
}

/// What the text before a `.` refers to.
enum Receiver {
    /// A user object or form: offer its methods, members and gadgets.
    User { name: String, is_form: bool },
    /// A builtin type such as STRING or BUTTON: offer matching builtin methods.
    Builtin(String),
    Unknown,
}

struct Backend {
    client: Client,
    /// Open documents, with their latest text and symbols.
    documents: DashMap<Url, Document>,
    /// Symbols of every indexed file in the workspace (open or not).
    workspace: Arc<DashMap<Url, Arc<SymbolTable>>>,
    /// Directories already indexed, so single-file mode indexes each folder once.
    indexed_dirs: Mutex<HashSet<PathBuf>>,
    roots: Mutex<Vec<PathBuf>>,
    builtins: Arc<Vec<Builtin>>,
    parser: Arc<PmlParser>,
}

impl Backend {
    /// Parse, extract symbols, publish diagnostics. Called on open and change.
    async fn update(&self, uri: Url, text: String) {
        let tree = self.parser.parse(&text);
        let (symbols, diags) = match &tree {
            Some(t) => (SymbolTable::extract(t, &text), diagnostics::collect(t, &text)),
            None => (SymbolTable::default(), Vec::new()),
        };
        let symbols = Arc::new(symbols);
        self.workspace.insert(uri.clone(), symbols.clone());
        self.documents.insert(uri.clone(), Document { text, symbols });
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    /// Index `dir` on a worker thread and merge the result into the workspace.
    async fn index_directory(&self, dir: PathBuf) {
        if !self.indexed_dirs.lock().unwrap().insert(dir.clone()) {
            return;
        }
        let workspace = self.workspace.clone();
        let open: HashSet<Url> = self.documents.iter().map(|d| d.key().clone()).collect();
        let shown = dir.display().to_string();
        let count = tokio::task::spawn_blocking(move || {
            let indexed = workspace::index_dir(&dir);
            let n = indexed.len();
            for (url, table) in indexed {
                // Open documents already have fresher symbols.
                if !open.contains(&url) {
                    workspace.insert(url, Arc::new(table));
                }
            }
            n
        })
        .await
        .unwrap_or(0);
        self.client
            .log_message(MessageType::INFO, format!("pml-lsp: indexed {} PML files under {}", count, shown))
            .await;
    }

    /// Re-index one file that changed on disk (and is not open).
    fn reindex_file(&self, uri: &Url) {
        if self.documents.contains_key(uri) {
            return;
        }
        let Ok(path) = uri.to_file_path() else { return };
        if !path.exists() {
            self.workspace.remove(uri);
            return;
        }
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&tree_sitter_pml::LANGUAGE.into()).is_err() {
            return;
        }
        if let Some((url, table)) = workspace::index_file(&mut parser, &path) {
            self.workspace.insert(url, Arc::new(table));
        }
    }

    /// Symbol tables to search: the current document first, then every other
    /// indexed file. Snapshot of Arcs, so no DashMap locks are held.
    fn tables(&self, current: &Url) -> Vec<(Url, Arc<SymbolTable>)> {
        let mut out = Vec::new();
        if let Some(doc) = self.documents.get(current) {
            out.push((current.clone(), doc.symbols.clone()));
        }
        let mut others: Vec<(Url, Arc<SymbolTable>)> = self
            .workspace
            .iter()
            .filter(|e| e.key() != current)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        others.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        out.extend(others);
        out
    }

    fn find_symbol(&self, current: &Url, word: &str) -> Option<(Url, Symbol)> {
        self.tables(current)
            .into_iter()
            .find_map(|(url, t)| t.find(word).cloned().map(|s| (url, s)))
    }

    /// Does any file define a user object or form named `name`?
    fn user_type(&self, tables: &[(Url, Arc<SymbolTable>)], name: &str) -> Option<Receiver> {
        tables.iter().find_map(|(_, t)| {
            t.symbols
                .iter()
                .find(|s| matches!(s.kind, SymbolKind::Object | SymbolKind::Form) && s.matches(name))
                .map(|s| Receiver::User { name: s.name.clone(), is_form: s.kind == SymbolKind::Form })
        })
    }

    /// Resolve the expression before a `.` by static type inference.
    fn resolve_receiver(&self, tables: &[(Url, Arc<SymbolTable>)], text: &str, symbols: &SymbolTable, receiver: &str) -> Receiver {
        let env = TypeEnv { tables, builtins: &self.builtins, text, symbols };
        match types::infer(receiver, &env) {
            Some(t) => self.user_type(tables, &t).unwrap_or(Receiver::Builtin(t)),
            None => Receiver::Unknown,
        }
    }

    /// Name sets for semantic tokens, from every indexed file plus builtins.
    fn semantic_names(&self, uri: &Url) -> semantic::Names {
        let mut n = semantic::Names::default();
        for (_, table) in self.tables(uri) {
            for sym in table.iter() {
                let key = sym.name.to_ascii_lowercase();
                match sym.kind {
                    SymbolKind::Function => n.functions.insert(key),
                    SymbolKind::Form => n.forms.insert(key),
                    SymbolKind::Variable => n.globals.insert(key),
                    SymbolKind::Method => n.methods.insert(key),
                    SymbolKind::Member => n.members.insert(key),
                    SymbolKind::Gadget => n.gadgets.insert(key),
                    SymbolKind::Object => n.objects.insert(key),
                };
            }
        }
        for b in self.builtins.iter() {
            let key = b.name.split('(').next().unwrap_or(&b.name).to_ascii_lowercase();
            match b.kind.to_ascii_lowercase().as_str() {
                "method" | "field" | "member" => {
                    n.builtin_methods.insert(key);
                }
                "constructor" | "class" | "object" => {
                    n.builtin_types.insert(key);
                }
                _ => {}
            }
            for r in &b.receiver_types {
                n.builtin_types.insert(r.to_ascii_lowercase());
            }
        }
        n
    }
}

/// The program as the type inference sees it: the current document plus every
/// indexed file and the builtins.
struct TypeEnv<'a> {
    tables: &'a [(Url, Arc<SymbolTable>)],
    builtins: &'a [Builtin],
    text: &'a str,
    symbols: &'a SymbolTable,
}

impl<'a> TypeEnv<'a> {
    fn find(&self, pred: impl Fn(&Symbol) -> bool) -> Option<Symbol> {
        self.tables.iter().find_map(|(_, t)| t.iter().find(|s| pred(s)).cloned())
    }
}

impl<'a> types::Env for TypeEnv<'a> {
    fn declared(&self, var: &str) -> Option<Decl> {
        context::declaration_of(self.text, var)
    }

    fn this_type(&self) -> Option<String> {
        self.symbols.owner().map(|o| o.name.clone())
    }

    fn global_type(&self, name: &str) -> Option<String> {
        self.find(|s| s.kind == SymbolKind::Form && s.matches(name)).map(|s| s.name)
    }

    fn member_type(&self, owner: &str, member: &str) -> Option<String> {
        if let Some(s) = self.find(|s| matches!(s.kind, SymbolKind::Member | SymbolKind::Gadget) && s.owned_by(owner) && s.matches(&format!(".{}", member))) {
            return s.type_name();
        }
        // `.val` of a gadget: the value type is not in builtins.json, so use
        // the usual type per gadget kind.
        if member.eq_ignore_ascii_case("val") {
            let v = match owner.to_ascii_uppercase().as_str() {
                "TEXT" | "TEXTPANE" | "PARAGRAPH" => Some("STRING"),
                "TOGGLE" => Some("BOOLEAN"),
                "OPTION" | "LIST" | "SLIDER" | "RGROUP" | "COMBO" | "NUMERIC" => Some("REAL"),
                _ => None,
            };
            if let Some(v) = v {
                return Some(v.to_string());
            }
        }
        // PML lets argument-less methods be written without parentheses
        // (`!this.name.fullName`), so a builtin method is a valid member too.
        self.method_return(owner, member)
    }

    fn method_return(&self, owner: &str, method: &str) -> Option<String> {
        if let Some(m) = self.find(|s| s.kind == SymbolKind::Method && s.owned_by(owner) && s.matches(&format!(".{}", method))) {
            return types::return_type_of(&m.detail);
        }
        self.builtins
            .iter()
            .filter(|b| b.is_member() && b.applies_to(Some(owner)))
            .find(|b| b.name.split('(').next().unwrap_or(&b.name).eq_ignore_ascii_case(method))
            .and_then(|b| types::return_type_of(&b.detail))
    }

    fn function_return(&self, name: &str) -> Option<String> {
        self.find(|s| s.kind == SymbolKind::Function && s.matches(name))
            .and_then(|s| types::return_type_of(&s.detail))
    }
}

impl Backend {
    /// Indentation edits for an open document, honouring the client's options.
    fn format_edits(&self, uri: &Url, options: &FormattingOptions, lines: Option<(u32, u32)>) -> Option<Vec<TextEdit>> {
        let doc = self.snapshot(uri)?;
        let tree = self.parser.parse(&doc.text)?;
        let indent = if options.insert_spaces { " ".repeat(options.tab_size.max(1) as usize) } else { "\t".to_string() };
        Some(format::edits(&tree, &doc.text, &indent, lines))
    }

    /// Copy of an open document, so no DashMap guard is held across other lookups.
    fn snapshot(&self, uri: &Url) -> Option<Document> {
        self.documents.get(uri).map(|d| d.clone())
    }

    /// Text of a file: the open buffer if there is one, else the file on disk.
    fn file_text(&self, url: &Url) -> Option<String> {
        if let Some(doc) = self.documents.get(url) {
            return Some(doc.text.clone());
        }
        let path = url.to_file_path().ok()?;
        std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Every occurrence of `word`, grouped by file. Locals (`!x`) are looked
    /// up in the current file only, inside the enclosing function or method.
    fn occurrences(&self, uri: &Url, pos: Position, word: &str) -> Vec<(Url, Vec<Range>)> {
        let mut out = Vec::new();
        if references::namespace_of(word) == references::Namespace::Local {
            let Some(doc) = self.documents.get(uri) else { return out };
            let scope = doc
                .symbols
                .symbols
                .iter()
                .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
                .find(|s| s.range.start <= pos && pos <= s.range.end)
                .map(|s| (s.range.start.line, s.range.end.line));
            let ranges = references::find(&doc.text, word, scope);
            if !ranges.is_empty() {
                out.push((uri.clone(), ranges));
            }
            return out;
        }
        let mut urls: Vec<Url> = self.workspace.iter().map(|e| e.key().clone()).collect();
        if !urls.contains(uri) {
            urls.push(uri.clone());
        }
        urls.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for url in urls {
            let Some(text) = self.file_text(&url) else { continue };
            let ranges = references::find(&text, word, None);
            if !ranges.is_empty() {
                out.push((url, ranges));
            }
        }
        out
    }

    /// Signature for the callee of a call site: user function/method/object
    /// constructor first, then builtins.
    fn signature_for(&self, uri: &Url, callee: &str) -> Option<SignatureInformation> {
        if let Some(type_name) = callee.strip_prefix("object ") {
            // Constructor: a user method named like its object, else `object TYPE()`.
            let tables = self.tables(uri);
            if let Some(obj) = tables.iter().find_map(|(_, t)| {
                t.symbols.iter().find(|s| s.kind == SymbolKind::Object && s.matches(type_name)).cloned()
            }) {
                let ctor = tables.iter().find_map(|(_, t)| {
                    t.iter()
                        .find(|s| s.kind == SymbolKind::Method && s.owned_by(&obj.name) && s.bare_name().eq_ignore_ascii_case(type_name))
                        .cloned()
                });
                return Some(match ctor {
                    Some(m) => signature::signature_from_detail(&m.detail.replace(&m.name, &format!("object {}", obj.name)), &m.documentation),
                    None => signature::signature_from_detail(&format!("object {}()", obj.name), &obj.documentation),
                });
            }
            return self
                .builtins
                .iter()
                .find(|b| b.kind.eq_ignore_ascii_case("constructor") && b.name.eq_ignore_ascii_case(type_name))
                .map(|b| signature::signature_from_detail(&b.detail, &b.documentation));
        }
        if callee.starts_with(['!', '.']) {
            if let Some((_, sym)) = self.find_symbol(uri, callee) {
                if matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                    return Some(signature::signature_from_detail(&sym.detail, &sym.documentation));
                }
            }
        }
        let bare = callee.trim_start_matches(['!', '.', '$']);
        self.builtins
            .iter()
            .filter(|b| matches!(b.kind.to_ascii_lowercase().as_str(), "method" | "function" | "constructor"))
            .find(|b| b.name.split('(').next().unwrap_or(&b.name).eq_ignore_ascii_case(bare))
            .map(|b| signature::signature_from_detail(&b.detail, &b.documentation))
    }
}

fn symbol_completion_kind(k: SymbolKind) -> CompletionItemKind {
    match k {
        SymbolKind::Function => CompletionItemKind::FUNCTION,
        SymbolKind::Method => CompletionItemKind::METHOD,
        SymbolKind::Object => CompletionItemKind::CLASS,
        SymbolKind::Member => CompletionItemKind::FIELD,
        SymbolKind::Form => CompletionItemKind::CLASS,
        SymbolKind::Gadget => CompletionItemKind::PROPERTY,
        SymbolKind::Variable => CompletionItemKind::VARIABLE,
    }
}

fn symbol_document_kind(k: SymbolKind) -> tower_lsp::lsp_types::SymbolKind {
    use tower_lsp::lsp_types::SymbolKind as K;
    match k {
        SymbolKind::Function => K::FUNCTION,
        SymbolKind::Method => K::METHOD,
        SymbolKind::Object => K::CLASS,
        SymbolKind::Member => K::FIELD,
        SymbolKind::Form => K::STRUCT,
        SymbolKind::Gadget => K::PROPERTY,
        SymbolKind::Variable => K::VARIABLE,
    }
}

fn file_name(url: &Url) -> String {
    url.path_segments()
        .and_then(|s| s.last())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Completion item for a user symbol. `label` is what the user sees and what
/// gets inserted over `replace` (so typed prefixes like `!!` or `.` are
/// replaced consistently). `from_file` is shown for symbols of other files.
fn symbol_completion(sym: &Symbol, label: String, replace: Range, from_file: Option<&Url>) -> CompletionItem {
    let rank = if from_file.is_some() { '1' } else { '0' };
    CompletionItem {
        kind: Some(symbol_completion_kind(sym.kind)),
        detail: Some(sym.detail.clone()),
        documentation: Some(markdown(&sym.documentation)),
        label_details: from_file.map(|u| CompletionItemLabelDetails {
            detail: None,
            description: Some(file_name(u)),
        }),
        sort_text: Some(format!("{}{}", rank, label.to_ascii_lowercase())),
        filter_text: Some(label.clone()),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(replace, label.clone()))),
        label,
        ..Default::default()
    }
}

#[allow(deprecated)] // DocumentSymbol::deprecated is required by the struct
fn to_document_symbol(sym: &Symbol) -> DocumentSymbol {
    DocumentSymbol {
        name: sym.name.clone(),
        detail: Some(sym.detail.clone()),
        kind: symbol_document_kind(sym.kind),
        tags: None,
        deprecated: None,
        range: sym.range,
        selection_range: sym.selection_range,
        children: if sym.children.is_empty() {
            None
        } else {
            Some(sym.children.iter().map(to_document_symbol).collect())
        },
    }
}

#[allow(deprecated)]
fn to_symbol_information(url: &Url, sym: &Symbol) -> SymbolInformation {
    SymbolInformation {
        name: sym.name.clone(),
        kind: symbol_document_kind(sym.kind),
        tags: None,
        deprecated: None,
        location: Location::new(url.clone(), sym.selection_range),
        container_name: sym.owner.clone(),
    }
}

fn make_hover(detail: &str, documentation: &str, from_file: Option<&Url>) -> Hover {
    let mut value = format!("```pml\n{}\n```\n\n{}", detail, documentation);
    if let Some(u) = from_file {
        value.push_str(&format!("\n\n*Defined in {}*", file_name(u)));
    }
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

// =============================================================================
// LSP
// =============================================================================

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let mut roots = Vec::new();
        if let Some(folders) = &params.workspace_folders {
            roots.extend(folders.iter().filter_map(|f| f.uri.to_file_path().ok()));
        }
        #[allow(deprecated)]
        if roots.is_empty() {
            if let Some(root) = params.root_uri.as_ref().and_then(|u| u.to_file_path().ok()) {
                roots.push(root);
            }
        }
        *self.roots.lock().unwrap() = roots;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), "!".to_string()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![",".to_string()]),
                    work_done_progress_options: Default::default(),
                }),
                references_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                    code_action_kinds: Some(vec![
                        CodeActionKind::QUICKFIX,
                        CodeActionKind::REFACTOR,
                        CodeActionKind::SOURCE_FIX_ALL,
                    ]),
                    ..Default::default()
                })),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                    SemanticTokensOptions {
                        legend: SemanticTokensLegend {
                            token_types: semantic::TOKEN_TYPES.to_vec(),
                            token_modifiers: semantic::TOKEN_MODIFIERS.to_vec(),
                        },
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        range: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
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
                format!("pml-lsp v{} initialized with {} builtins", env!("CARGO_PKG_VERSION"), self.builtins.len()),
            )
            .await;

        // Keep the index fresh when files change outside the editor.
        let watcher = Registration {
            id: "pml-lsp-watch".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(
                serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                    watchers: vec![FileSystemWatcher {
                        glob_pattern: GlobPattern::String(workspace::WATCH_GLOB.to_string()),
                        kind: None,
                    }],
                })
                .unwrap(),
            ),
        };
        let _ = self.client.register_capability(vec![watcher]).await;

        let roots = self.roots.lock().unwrap().clone();
        for root in roots {
            self.index_directory(root).await;
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        self.update(uri.clone(), params.text_document.text).await;
        // Single-file mode (no workspace root): index the file's folder.
        if self.roots.lock().unwrap().is_empty() {
            if let Some(dir) = uri.to_file_path().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
                self.index_directory(dir).await;
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // FULL sync: the last change carries the whole document.
        if let Some(change) = params.content_changes.into_iter().last() {
            self.update(params.text_document.uri, change.text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        // The workspace index keeps the last parsed symbols of the file.
        self.documents.remove(&params.text_document.uri);
        self.client.publish_diagnostics(params.text_document.uri, Vec::new(), None).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            self.reindex_file(&change.uri);
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let Some(doc) = self.snapshot(uri) else {
            return Ok(None);
        };
        let cursor = context::at(&doc.text, pos);
        let tables = self.tables(uri);
        let mut items: Vec<CompletionItem> = Vec::new();

        // Push a user symbol, tagging it with its file when it is not the current one.
        let push = |items: &mut Vec<CompletionItem>, url: &Url, sym: &Symbol, label: String| {
            let from = (url != uri).then_some(url);
            items.push(symbol_completion(sym, label, cursor.replace, from));
        };

        match &cursor.context {
            Context::Member { receiver } => {
                let resolved = self.resolve_receiver(&tables, &doc.text, &doc.symbols, receiver);
                for (url, table) in &tables {
                    for sym in table.iter() {
                        let offer = match (&resolved, sym.kind) {
                            (Receiver::User { name, .. }, SymbolKind::Method | SymbolKind::Member | SymbolKind::Gadget) => {
                                sym.owned_by(name)
                            }
                            (Receiver::Builtin(_), _) => false,
                            (Receiver::Unknown, SymbolKind::Method | SymbolKind::Member | SymbolKind::Gadget) => true,
                            _ => false,
                        };
                        if offer {
                            push(&mut items, url, sym, sym.bare_name().to_string());
                        }
                    }
                }
                let builtin_filter: Option<String> = match &resolved {
                    Receiver::User { is_form: true, .. } => Some("FORM".to_string()),
                    Receiver::User { is_form: false, .. } => None,
                    Receiver::Builtin(t) => Some(t.clone()),
                    Receiver::Unknown => None,
                };
                let offer_builtins = !matches!(resolved, Receiver::User { is_form: false, .. });
                if offer_builtins {
                    items.extend(
                        self.builtins
                            .iter()
                            .filter(|b| b.is_member() && b.applies_to(builtin_filter.as_deref()))
                            .map(|b| b.to_completion()),
                    );
                }
            }
            Context::Variable { global } => {
                if *global {
                    for (url, table) in &tables {
                        for sym in table
                            .iter()
                            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Form | SymbolKind::Variable))
                        {
                            push(&mut items, url, sym, sym.name.clone());
                        }
                    }
                }
                // Every variable mentioned in the file, so locals complete too.
                let known: HashSet<String> = items.iter().map(|i| i.label.to_ascii_lowercase()).collect();
                for name in context::variables_in(&doc.text) {
                    let is_global = name.starts_with("!!");
                    if is_global != *global || known.contains(&name.to_ascii_lowercase()) {
                        continue;
                    }
                    items.push(CompletionItem {
                        label: name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some(if is_global { "global variable" } else { "variable" }.to_string()),
                        sort_text: Some(format!("0{}", name.to_ascii_lowercase())),
                        filter_text: Some(name.clone()),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(cursor.replace, name))),
                        ..Default::default()
                    });
                }
            }
            Context::Statement => {
                for (url, table) in &tables {
                    for sym in table
                        .iter()
                        .filter(|s| matches!(s.kind, SymbolKind::Object | SymbolKind::Function | SymbolKind::Form))
                    {
                        push(&mut items, url, sym, sym.name.clone());
                    }
                }
                items.extend(self.builtins.iter().filter(|b| !b.is_member()).map(|b| b.to_completion()));
            }
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(doc) = self.snapshot(&uri) else {
            return Ok(None);
        };
        let Some(word) = context::word_at(&doc.text, pos) else {
            return Ok(None);
        };

        // User symbols first (current file, then workspace): most specific wins.
        if let Some((url, sym)) = self.find_symbol(&uri, &word) {
            let from = (url != uri).then_some(&url);
            return Ok(Some(make_hover(&sym.detail, &sym.documentation, from)));
        }
        let bare = word.trim_start_matches(['!', '.', '$']);
        for b in self.builtins.iter() {
            let ident = b.name.split('(').next().unwrap_or(&b.name);
            if ident.eq_ignore_ascii_case(bare) {
                return Ok(Some(make_hover(&b.detail, &b.documentation, None)));
            }
        }
        // A variable: show its inferred type.
        if word.trim_start_matches('$').starts_with('!') {
            let tables = self.tables(&uri);
            let env = TypeEnv { tables: &tables, builtins: &self.builtins, text: &doc.text, symbols: &doc.symbols };
            if let Some(t) = types::infer(&word, &env) {
                return Ok(Some(make_hover(&format!("{} is {}", word, t), "*Inferred type*", None)));
            }
        }
        Ok(None)
    }

    async fn goto_definition(&self, params: GotoDefinitionParams) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(word) = self.documents.get(&uri).and_then(|d| context::word_at(&d.text, pos)) else {
            return Ok(None);
        };
        Ok(self
            .find_symbol(&uri, &word)
            .map(|(url, sym)| GotoDefinitionResponse::Scalar(Location::new(url, sym.selection_range))))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(site) = self.documents.get(&uri).and_then(|d| signature::call_at(&d.text, pos)) else {
            return Ok(None);
        };
        let Some(sig) = self.signature_for(&uri, &site.callee) else {
            return Ok(None);
        };
        let n_params = sig.parameters.as_ref().map(|p| p.len() as u32).unwrap_or(0);
        Ok(Some(SignatureHelp {
            active_parameter: Some(site.active_parameter.min(n_params.saturating_sub(1))),
            signatures: vec![sig],
            active_signature: Some(0),
        }))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let Some(word) = self.documents.get(&uri).and_then(|d| context::word_at(&d.text, pos)) else {
            return Ok(None);
        };
        let locations: Vec<Location> = self
            .occurrences(&uri, pos, &word)
            .into_iter()
            .flat_map(|(url, ranges)| ranges.into_iter().map(move |r| Location::new(url.clone(), r)))
            .collect();
        Ok(Some(locations))
    }

    async fn prepare_rename(&self, params: TextDocumentPositionParams) -> Result<Option<PrepareRenameResponse>> {
        let Some((word, range)) = self.documents.get(&params.text_document.uri).and_then(|d| context::word_span_at(&d.text, params.position)) else {
            return Ok(None);
        };
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range,
            placeholder: references::bare(&word).to_string(),
        }))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let Some(word) = self.documents.get(&uri).and_then(|d| context::word_at(&d.text, pos)) else {
            return Ok(None);
        };
        let Some(new_name) = references::new_bare_name(&params.new_name) else {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(format!(
                "`{}` is not a valid PML name",
                params.new_name
            )));
        };
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (url, ranges) in self.occurrences(&uri, pos, &word) {
            changes.insert(url, ranges.into_iter().map(|r| TextEdit::new(r, new_name.clone())).collect());
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        Ok(self.format_edits(&params.text_document.uri, &params.options, None))
    }

    async fn range_formatting(&self, params: DocumentRangeFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let lines = (params.range.start.line, params.range.end.line);
        Ok(self.format_edits(&params.text_document.uri, &params.options, Some(lines)))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.snapshot(&uri) else {
            return Ok(None);
        };
        let row = params.range.start.line;
        let Some(line) = doc.text.lines().nth(row as usize) else {
            return Ok(None);
        };
        let indent = "  ";
        let mut out: Vec<CodeActionOrCommand> = Vec::new();

        // Create a stub for an undefined method called on this line.
        if let Some((word, _)) = context::word_span_at(&doc.text, params.range.start) {
            if word.starts_with('.') {
                let tables = self.tables(&uri);
                let owner = doc.symbols.owner().map(|o| o.name.clone());
                let defined = tables.iter().any(|(_, t)| {
                    t.iter().any(|s| {
                        s.kind == SymbolKind::Method
                            && s.matches(&word)
                            && owner.as_deref().map(|o| s.owned_by(o)).unwrap_or(true)
                    })
                });
                let is_call = line.to_ascii_lowercase().contains(&format!("{}(", word.to_ascii_lowercase()));
                let is_builtin = self.builtins.iter().any(|b| {
                    b.is_member() && b.name.split('(').next().unwrap_or(&b.name).eq_ignore_ascii_case(word.trim_start_matches('.'))
                });
                if is_call && !defined && !is_builtin {
                    out.push(CodeActionOrCommand::CodeAction(actions::create_method(&uri, &doc.text, &word, line, indent)));
                }
            }
        }

        // Error handler after a plain statement.
        if actions::is_handleable(line) {
            let next = doc.text.lines().nth(row as usize + 1).map(|l| l.trim().to_ascii_lowercase());
            if !next.map(|n| n.starts_with("handle")).unwrap_or(false) {
                out.push(CodeActionOrCommand::CodeAction(actions::add_handle(&uri, line, row, indent)));
            }
        }

        // Whole-file indentation fix.
        let options = FormattingOptions { tab_size: 2, insert_spaces: true, ..Default::default() };
        if let Some(action) = self.format_edits(&uri, &options, None).and_then(|e| actions::format_document(&uri, e)) {
            out.push(CodeActionOrCommand::CodeAction(action));
        }

        Ok(Some(out))
    }

    async fn semantic_tokens_full(&self, params: SemanticTokensParams) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.snapshot(&uri) else {
            return Ok(None);
        };
        let names = self.semantic_names(&uri);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: semantic::tokens(&doc.text, &names),
        })))
    }

    async fn document_symbol(&self, params: DocumentSymbolParams) -> Result<Option<DocumentSymbolResponse>> {
        let Some(doc) = self.documents.get(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(DocumentSymbolResponse::Nested(
            doc.symbols.symbols.iter().map(to_document_symbol).collect(),
        )))
    }

    async fn symbol(&self, params: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.trim_start_matches(['!', '.']).to_ascii_lowercase();
        let mut out = Vec::new();
        for entry in self.workspace.iter() {
            for sym in entry.value().iter() {
                if query.is_empty() || sym.bare_name().to_ascii_lowercase().contains(&query) {
                    out.push(to_symbol_information(entry.key(), sym));
                }
            }
            if out.len() > 2000 {
                break;
            }
        }
        Ok(Some(out))
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
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
        workspace: Arc::new(DashMap::new()),
        indexed_dirs: Mutex::new(HashSet::new()),
        roots: Mutex::new(Vec::new()),
        builtins: builtins.clone(),
        parser: Arc::new(PmlParser::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
