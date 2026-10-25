//! Symbol extraction from a parsed PML document.
//!
//! Walks the tree-sitter parse tree and collects every user-defined symbol:
//! functions, methods, objects and their members, forms and their gadgets,
//! and global variable assignments. Node kinds and field names come from the
//! `tree-sitter-pml` grammar (`src/node-types.json` in that repo).

use tower_lsp::lsp_types::{Position, Range};
use tree_sitter::{Node, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Object,
    Member,
    Form,
    Gadget,
    Variable,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    /// Name as the user writes it: `!!Area`, `.Show` (methods and members
    /// keep their leading dot), `MYOBJECT`, `!!myForm`.
    pub name: String,
    pub kind: SymbolKind,
    /// Short signature shown next to the name in the completion menu
    /// (e.g. `define function !!Area(!Length is REAL) is REAL`).
    pub detail: String,
    /// Markdown documentation: the leading `--` comment block if present,
    /// otherwise a synthesized note.
    pub documentation: String,
    /// Full extent of the definition.
    pub range: Range,
    /// Extent of the name token, used for go-to-definition.
    pub selection_range: Range,
    /// Members of an object, gadgets of a form.
    pub children: Vec<Symbol>,
    /// For methods, members and gadgets: the object or form they belong to
    /// (`BOX`, `!!myForm`). Methods in a `.pmlobj`/`.pmlfrm` file belong to
    /// the object or form defined earlier in the same file.
    pub owner: Option<String>,
}

impl Symbol {
    /// Name without the `.`/`!`/`!!`/`$` prefix, for loose matching.
    pub fn bare_name(&self) -> &str {
        self.name.trim_start_matches(['!', '.', '$'])
    }

    /// True when `word` (as typed under the cursor) refers to this symbol.
    /// The prefix decides the namespace: `!!x` only matches globals and
    /// functions, `!x` locals, `.x` methods/members/gadgets, and a bare word
    /// only unprefixed names (objects).
    pub fn matches(&self, word: &str) -> bool {
        let word = word.trim_start_matches('$');
        let w = word.trim_start_matches(['!', '.']);
        if w.is_empty() || !self.bare_name().eq_ignore_ascii_case(w) {
            return false;
        }
        let prefix_of = |s: &str| -> String { s.chars().take_while(|c| matches!(c, '!' | '.')).collect() };
        prefix_of(word) == prefix_of(&self.name)
    }

    /// True when this symbol belongs to `owner` (object or form name).
    pub fn owned_by(&self, owner: &str) -> bool {
        self.owner
            .as_deref()
            .map(|o| o.trim_start_matches('!').eq_ignore_ascii_case(owner.trim_start_matches('!')))
            .unwrap_or(false)
    }

    /// Uppercase type for gadgets (`BUTTON`), members (`REAL`), forms (`FORM`).
    pub fn type_name(&self) -> Option<String> {
        match self.kind {
            SymbolKind::Form => Some("FORM".to_string()),
            SymbolKind::Gadget | SymbolKind::Member => self
                .detail
                .rsplit(" is ")
                .next()
                .map(|t| t.trim().to_ascii_uppercase()),
            _ => None,
        }
    }
}

#[derive(Default, Debug)]
pub struct SymbolTable {
    pub symbols: Vec<Symbol>,
}

impl SymbolTable {
    /// Build a SymbolTable from a parsed tree and the original source text.
    pub fn extract(tree: &Tree, source: &str) -> Self {
        let mut table = SymbolTable::default();
        let mut owner = None;
        walk(tree.root_node(), source.as_bytes(), &mut table.symbols, &mut owner);
        table
    }

    /// The object or form this file defines, if any.
    pub fn owner(&self) -> Option<&Symbol> {
        self.symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Object | SymbolKind::Form))
    }

    /// Every symbol, including children, depth first.
    pub fn iter(&self) -> impl Iterator<Item = &Symbol> {
        fn push<'a>(out: &mut Vec<&'a Symbol>, syms: &'a [Symbol]) {
            for s in syms {
                out.push(s);
                push(out, &s.children);
            }
        }
        let mut all = Vec::new();
        push(&mut all, &self.symbols);
        all.into_iter()
    }

    /// First symbol whose name matches `word`.
    pub fn find(&self, word: &str) -> Option<&Symbol> {
        self.iter().find(|s| s.matches(word))
    }
}

/// Gadget commands that define a `.name` inside a form.
const GADGET_KINDS: &[&str] = &[
    "button", "text", "toggle", "option", "paragraph", "list", "textpane", "slider",
    "rgroup", "combo", "numeric", "line", "selector", "database", "frame", "view",
    "menu", "bar", "tabset", "panel", "container",
];

fn walk(node: Node, src: &[u8], out: &mut Vec<Symbol>, owner: &mut Option<String>) {
    match node.kind() {
        "function_definition" => {
            out.extend(extract_function(node, src));
            return;
        }
        "method_definition" => {
            out.extend(extract_method(node, src).map(|mut m| {
                m.owner = owner.clone();
                m
            }));
            return;
        }
        "object_definition" => {
            if let Some(obj) = extract_object(node, src) {
                *owner = Some(obj.name.clone());
                out.push(obj);
            }
            return;
        }
        "form_definition" => {
            if let Some(form) = extract_form(node, src) {
                *owner = Some(form.name.clone());
                out.push(form);
            }
            return;
        }
        // Only global variables (`!!name = ...`, `var !!name ...`) are offered.
        "assignment" => out.extend(extract_global(node, src, "left")),
        "var_statement" => out.extend(extract_global(node, src, "variable")),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, src, out, owner);
    }
}

fn node_text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn to_position(p: tree_sitter::Point) -> Position {
    Position::new(p.row as u32, p.column as u32)
}

fn node_range(node: Node) -> Range {
    Range::new(to_position(node.start_position()), to_position(node.end_position()))
}

fn child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

fn children_by_kind<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == kind)
        .collect()
}

/// Leading `--` / `$*` comment lines directly above the node, joined.
fn leading_comment(node: Node, src: &[u8]) -> String {
    let mut comments: Vec<String> = Vec::new();
    let mut current = node.prev_named_sibling();
    let mut next_row = node.start_position().row;
    while let Some(sib) = current {
        if sib.kind() != "comment" || sib.end_position().row + 1 < next_row {
            break;
        }
        let text = node_text(sib, src);
        let cleaned = text
            .trim_start_matches("--")
            .trim_start_matches("$*")
            .trim()
            .to_string();
        comments.push(cleaned);
        next_row = sib.start_position().row;
        current = sib.prev_named_sibling();
    }
    comments.reverse();
    comments.join("\n")
}

fn with_comment(comment: &str, fallback: &str, detail: &str) -> String {
    let intro = if comment.is_empty() { fallback } else { comment };
    format!("{}\n\n```pml\n{}\n```", intro, detail)
}

fn extract_function(node: Node, src: &[u8]) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| child_by_kind(node, "macro_function"))?;
    let name = node_text(name_node, src);
    let detail = format!(
        "define function {}({}){}",
        name,
        format_params(node, src),
        format_return_type(node, src)
    );
    Some(Symbol {
        documentation: with_comment(
            &leading_comment(node, src),
            "**User-defined function** in this file.",
            &detail,
        ),
        name,
        kind: SymbolKind::Function,
        detail,
        range: node_range(node),
        selection_range: node_range(name_node),
        children: Vec::new(),
        owner: None,
    })
}

fn extract_method(node: Node, src: &[u8]) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| child_by_kind(node, "method_identifier"))?;
    let name = node_text(name_node, src);
    let detail = format!(
        "define method {}({}){}",
        name,
        format_params(node, src),
        format_return_type(node, src)
    );
    Some(Symbol {
        documentation: with_comment(
            &leading_comment(node, src),
            "**User-defined method** in this file.",
            &detail,
        ),
        name,
        kind: SymbolKind::Method,
        detail,
        range: node_range(node),
        selection_range: node_range(name_node),
        children: Vec::new(),
        owner: None,
    })
}

fn extract_member(node: Node, src: &[u8]) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| child_by_kind(node, "member_name"))?;
    let name = node_text(name_node, src);
    let type_str = node
        .child_by_field_name("type")
        .or_else(|| child_by_kind(node, "type_identifier"))
        .map(|n| node_text(n, src))
        .unwrap_or_else(|| "ANY".to_string());
    let detail = format!("member {} is {}", name, type_str);
    Some(Symbol {
        documentation: format!("**Object member.**\n\n```pml\n{}\n```", detail),
        name,
        kind: SymbolKind::Member,
        detail,
        range: node_range(node),
        selection_range: node_range(name_node),
        children: Vec::new(),
        owner: None,
    })
}

fn extract_object(node: Node, src: &[u8]) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| child_by_kind(node, "identifier"))?;
    let name = node_text(name_node, src);
    let children: Vec<Symbol> = children_by_kind(node, "member_definition")
        .into_iter()
        .filter_map(|m| extract_member(m, src))
        .map(|mut m| {
            m.owner = Some(name.clone());
            m
        })
        .collect();

    let detail = format!("define object {}", name);
    let comment = leading_comment(node, src);
    let mut doc = vec![if comment.is_empty() {
        "**User-defined object type.**".to_string()
    } else {
        comment
    }];
    if !children.is_empty() {
        doc.push("**Members:**".to_string());
        doc.push(
            children
                .iter()
                .map(|c| format!("- `{}`", c.detail.trim_start_matches("member ")))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    Some(Symbol {
        name,
        kind: SymbolKind::Object,
        detail,
        documentation: doc.join("\n\n"),
        range: node_range(node),
        selection_range: node_range(name_node),
        children,
        owner: None,
    })
}

fn extract_form(node: Node, src: &[u8]) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| child_by_kind(node, "variable"))?;
    let name = node_text(name_node, src);

    let mut children = Vec::new();
    collect_gadgets(node, src, &mut children);
    for c in &mut children {
        c.owner = Some(name.clone());
    }

    let detail = format!("setup form {}", name);
    let comment = leading_comment(node, src);
    let mut doc = vec![if comment.is_empty() {
        "**Form definition.**".to_string()
    } else {
        comment
    }];
    if !children.is_empty() {
        doc.push("**Gadgets and members:**".to_string());
        doc.push(
            children
                .iter()
                .map(|c| format!("- `{}`", c.detail))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    Some(Symbol {
        name,
        kind: SymbolKind::Form,
        detail,
        documentation: doc.join("\n\n"),
        range: node_range(node),
        selection_range: node_range(name_node),
        children,
        owner: None,
    })
}

/// Collect `member .x is TYPE` lines and gadget definitions (`button .ok ...`,
/// `frame .fr ...`) anywhere inside a form body, including nested frames.
fn collect_gadgets(node: Node, src: &[u8], out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "member_definition" => out.extend(extract_member(child, src)),
            "command_statement" => out.extend(extract_gadget(child, src, None)),
            "frame_block" => {
                let kind = child
                    .child_by_field_name("kind")
                    .map(|k| node_text(k, src))
                    .unwrap_or_else(|| "frame".to_string());
                out.extend(extract_gadget(child, src, Some(&kind)));
                collect_gadgets(child, src, out);
            }
            _ => {}
        }
    }
}

/// A gadget line is `<gadget-kind> .name ...`. `kind_override` is used for
/// frame blocks, whose kind is a keyword rather than a command name.
fn extract_gadget(node: Node, src: &[u8], kind_override: Option<&str>) -> Option<Symbol> {
    let kind = match kind_override {
        Some(k) => k.to_ascii_lowercase(),
        None => node_text(node.child_by_field_name("name")?, src).to_ascii_lowercase(),
    };
    if !GADGET_KINDS.contains(&kind.as_str()) {
        return None;
    }
    // The first `.name` word after the gadget kind is the gadget name.
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| {
        c.kind() == "command_word"
            && c.utf8_text(src).map(|t| t.starts_with('.')).unwrap_or(false)
    });
    let name_node = found?;
    let name = node_text(name_node, src);
    let detail = format!("{} {} is {}", kind, name, kind.to_ascii_uppercase());
    Some(Symbol {
        documentation: format!(
            "**Form gadget.**\n\n```pml\n{}\n```",
            node_text(node, src).lines().next().unwrap_or("").trim()
        ),
        name,
        kind: SymbolKind::Gadget,
        detail,
        range: node_range(node),
        selection_range: node_range(name_node),
        children: Vec::new(),
        owner: None,
    })
}

/// `!!name = ...` or `var !!name ...` at any level.
fn extract_global(node: Node, src: &[u8], field: &str) -> Option<Symbol> {
    let var_node = node.child_by_field_name(field)?;
    if var_node.kind() != "variable" {
        return None;
    }
    let name = node_text(var_node, src);
    if !name.starts_with("!!") {
        return None;
    }
    let detail = format!("{} (global variable)", name);
    Some(Symbol {
        documentation: format!(
            "**Global variable** assigned in this file.\n\n```pml\n{}\n```",
            node_text(node, src).trim()
        ),
        name,
        kind: SymbolKind::Variable,
        detail,
        range: node_range(node),
        selection_range: node_range(var_node),
        children: Vec::new(),
        owner: None,
    })
}

/// `!a is REAL, !b is STRING`
fn format_params(def_node: Node, src: &[u8]) -> String {
    let Some(plist) = child_by_kind(def_node, "parameter_list") else {
        return String::new();
    };
    children_by_kind(plist, "parameter")
        .into_iter()
        .map(|p| {
            let pname = p
                .child_by_field_name("name")
                .map(|n| node_text(n, src))
                .unwrap_or_else(|| "?".to_string());
            let ptype = p
                .child_by_field_name("type")
                .map(|n| node_text(n, src))
                .unwrap_or_else(|| "ANY".to_string());
            format!("{} is {}", pname, ptype)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// ` is RETURNTYPE` or empty. The return type is the `type_identifier` that is
/// a direct child of the definition (parameter types sit inside `parameter`).
fn format_return_type(def_node: Node, src: &[u8]) -> String {
    child_by_kind(def_node, "type_identifier")
        .map(|t| format!(" is {}", node_text(t, src)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::PmlParser;

    fn extract_from(src: &str) -> SymbolTable {
        let parser = PmlParser::new();
        let tree = parser.parse(src).expect("parse failed");
        SymbolTable::extract(&tree, src)
    }

    #[test]
    fn extracts_function_definition_with_signature() {
        let table = extract_from(
            "-- Computes an area\n\
             define function !!Area(!Length is REAL, !Width is REAL) is REAL\n\
             !Area = !Length * !Width\n\
             return !Area\n\
             endfunction\n",
        );
        let func = table.find("!!Area").expect("function not extracted");
        assert_eq!(func.kind, SymbolKind::Function);
        assert_eq!(
            func.detail,
            "define function !!Area(!Length is REAL, !Width is REAL) is REAL"
        );
        assert!(func.documentation.starts_with("Computes an area"));
        assert_eq!(func.selection_range.start.line, 1);
    }

    #[test]
    fn skips_local_assignments_inside_function() {
        let table =
            extract_from("define function !!Foo() is REAL\n!x = 1\nreturn !x\nendfunction\n");
        assert!(table.find("!x").is_none());
    }

    #[test]
    fn extracts_global_assignment_and_var() {
        let table = extract_from("!!myGlobal = 'hello'\nvar !!coll coll all equi\n!local = 1\n");
        assert_eq!(table.find("!!myGlobal").unwrap().kind, SymbolKind::Variable);
        assert_eq!(table.find("!!coll").unwrap().kind, SymbolKind::Variable);
        assert!(table.find("!local").is_none());
        assert!(table.find("!myGlobal").is_none(), "!x must not match !!x");
        assert!(table.find("$!!myGlobal").is_some(), "$ dereference prefix is ignored");
    }

    #[test]
    fn extracts_method_and_object_members() {
        let table = extract_from(
            "define object BOX\n  member .width is REAL\n  member .height is REAL\nendobject\n\n\
             define method .area() is REAL\n  return !this.width * !this.height\nendmethod\n",
        );
        let obj = table.find("BOX").unwrap();
        assert_eq!(obj.kind, SymbolKind::Object);
        assert_eq!(obj.children.len(), 2);
        assert_eq!(obj.children[0].name, ".width");
        assert_eq!(obj.children[0].detail, "member .width is REAL");
        assert_eq!(obj.children[0].type_name().as_deref(), Some("REAL"));
        let m = table.find(".area").unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert!(table.find("area").is_none(), "bare word must not match a method");
        assert!(table.find("!!area").is_none(), "a global must not match a method");
        assert_eq!(m.detail, "define method .area() is REAL");
        assert_eq!(m.owner.as_deref(), Some("BOX"));
        assert!(m.owned_by("box"));
        assert_eq!(obj.children[0].owner.as_deref(), Some("BOX"));
        assert_eq!(table.owner().unwrap().name, "BOX");
        assert_eq!(table.find(".height").unwrap().kind, SymbolKind::Member);
    }

    #[test]
    fn extracts_form_gadgets_including_nested_frames() {
        let table = extract_from(
            "setup form !!myForm dialog dock left\n\
               title |My Form|\n\
               member .count is REAL\n\
               frame .fr |Frame|\n\
                 button .ok |OK| call |!this.okPressed()|\n\
               exit\n\
               text .input |Name| width 20 is STRING\n\
             exit\n",
        );
        let form = table.find("!!myForm").unwrap();
        assert_eq!(form.kind, SymbolKind::Form);
        assert_eq!(form.type_name().as_deref(), Some("FORM"));
        let names: Vec<&str> = form.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec![".count", ".fr", ".ok", ".input"]);
        assert_eq!(form.children[2].detail, "button .ok is BUTTON");
        assert_eq!(form.children[2].type_name().as_deref(), Some("BUTTON"));
        assert_eq!(form.children[1].kind, SymbolKind::Gadget);
        assert!(form.children[2].owned_by("!!myForm"));
    }
}
