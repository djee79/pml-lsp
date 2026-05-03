//! Symbol extraction from a parsed PML document.
//!
//! Walks the tree-sitter parse tree and collects every user-defined symbol —
//! functions, methods, objects, members, parameters, and top-level variable
//! assignments — into a `SymbolTable`. The node names referenced here come
//! directly from your grammar (verified against `:InspectTree` output and
//! the existing `locals.scm`).

use tree_sitter::{Node, Tree};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Object,
    Member,
    Parameter,
    Variable,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Short type signature shown next to the name in the completion menu
    /// (e.g., `"!!Area(!Length is REAL, !Width is REAL) is REAL"`).
    pub detail: String,
    /// Markdown documentation. For user code this is normally extracted
    /// from a leading comment block, or a synthesized signature note if
    /// no comment is present.
    pub documentation: String,
    /// 0-indexed start row in the source — used later for go-to-definition.
    pub start_line: usize,
}

#[derive(Default, Debug)]
pub struct SymbolTable {
    pub symbols: Vec<Symbol>,
}

impl SymbolTable {
    /// Build a SymbolTable from a parsed tree and the original source text.
    pub fn extract(tree: &Tree, source: &str) -> Self {
        let mut table = SymbolTable::default();
        let root = tree.root_node();
        let bytes = source.as_bytes();
        walk(root, bytes, &mut table);
        table
    }
}

fn walk(node: Node, src: &[u8], out: &mut SymbolTable) {
    match node.kind() {
        "function_definition" => {
            extract_function(node, src, out);
            // Don't recurse into the function body — its parameters and locals
            // aren't useful as global completions and would add noise.
            return;
        }
        "method_definition" => {
            extract_method(node, src, out);
            return;
        }
        "object_definition" => {
            extract_object(node, src, out);
            // Recurse so member_definition children get picked up.
        }
        "member_definition" => {
            extract_member(node, src, out);
            return;
        }
        "assignment" => {
            // Top-level assignments (e.g., `!!myGlobal = 'foo'`) are useful
            // as completion items. Local !x = 1 inside functions is filtered
            // out because we already returned above before descending into
            // function/method bodies.
            extract_assignment(node, src, out);
        }
        _ => {}
    }

    // Default: recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, src, out);
    }
}

fn node_text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

/// Find a child by node-kind name.
fn child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

/// Collect text of all named children matching one of the given kinds.
fn collect_children<'a>(node: Node<'a>, kinds: &[&str]) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            out.push(child);
        }
    }
    out
}

/// Extract leading `--` comment lines immediately above the node.
/// Returns the joined text or empty string if none found.
fn leading_comment(node: Node, src: &[u8]) -> String {
    // tree-sitter doesn't track preceding comments by default; we reach
    // backward through previous siblings looking for `comment` nodes.
    let mut comments: Vec<String> = Vec::new();
    let mut current = node.prev_named_sibling();
    let node_row = node.start_position().row;
    while let Some(sib) = current {
        // Only accept comments immediately above (within ~2 lines of the def).
        if sib.end_position().row + 2 < node_row {
            break;
        }
        if sib.kind() == "comment" {
            let text = node_text(sib, src);
            // Strip leading `--` and surrounding whitespace
            let cleaned = text
                .trim_start_matches('-')
                .trim_start_matches('-')
                .trim()
                .to_string();
            comments.push(cleaned);
            current = sib.prev_named_sibling();
        } else {
            break;
        }
    }
    comments.reverse();
    comments.join("\n")
}

fn extract_function(node: Node, src: &[u8], out: &mut SymbolTable) {
    // Grammar: function_definition has a `name: macro_function` child plus
    // an optional `parameter_list` and an `is_clause` for the return type.
    let Some(name_node) = node.child_by_field_name("name").or_else(|| child_by_kind(node, "macro_function")) else {
        return;
    };
    let name = node_text(name_node, src);

    let params = format_params(node, src);
    let return_type = format_return_type(node, src);
    let detail = format!("{}({}){}", name, params, return_type);

    let comment = leading_comment(node, src);
    let documentation = if comment.is_empty() {
        format!("**User-defined function** in this file.\n\n```pml\n{}\n```", detail)
    } else {
        format!("{}\n\n```pml\n{}\n```", comment, detail)
    };

    out.symbols.push(Symbol {
        name,
        kind: SymbolKind::Function,
        detail,
        documentation,
        start_line: node.start_position().row,
    });
}

fn extract_method(node: Node, src: &[u8], out: &mut SymbolTable) {
    // method_identifier holds the `.Name` form
    let Some(name_node) = child_by_kind(node, "method_identifier") else {
        return;
    };
    let name = node_text(name_node, src).trim_start_matches('.').to_string();

    let params = format_params(node, src);
    let return_type = format_return_type(node, src);
    let detail = format!(".{}({}){}", name, params, return_type);

    let comment = leading_comment(node, src);
    let documentation = if comment.is_empty() {
        format!("**User-defined method**.\n\n```pml\n{}\n```", detail)
    } else {
        format!("{}\n\n```pml\n{}\n```", comment, detail)
    };

    out.symbols.push(Symbol {
        name,
        kind: SymbolKind::Method,
        detail,
        documentation,
        start_line: node.start_position().row,
    });
}

fn extract_object(node: Node, src: &[u8], out: &mut SymbolTable) {
    let Some(name_node) = child_by_kind(node, "identifier") else {
        return;
    };
    let name = node_text(name_node, src);

    let members: Vec<String> = collect_children(node, &["member_definition"])
        .iter()
        .filter_map(|m| {
            let mname = child_by_kind(*m, "member_name").map(|n| node_text(n, src))?;
            let mtype = child_by_kind(*m, "type_identifier").map(|n| node_text(n, src))
                .unwrap_or_else(|| "ANY".to_string());
            Some(format!(".{} is {}", mname, mtype))
        })
        .collect();

    let detail = format!("define object {} ({} members)", name, members.len());
    let comment = leading_comment(node, src);

    let mut doc_parts = Vec::new();
    if !comment.is_empty() {
        doc_parts.push(comment);
    }
    doc_parts.push("**User-defined object type.**".to_string());
    if !members.is_empty() {
        doc_parts.push("**Members:**".to_string());
        for m in &members {
            doc_parts.push(format!("- `{}`", m));
        }
    }

    out.symbols.push(Symbol {
        name,
        kind: SymbolKind::Object,
        detail,
        documentation: doc_parts.join("\n\n"),
        start_line: node.start_position().row,
    });
}

fn extract_member(node: Node, src: &[u8], out: &mut SymbolTable) {
    let Some(name_node) = child_by_kind(node, "member_name") else {
        return;
    };
    let name = node_text(name_node, src);
    let type_str = child_by_kind(node, "type_identifier")
        .map(|n| node_text(n, src))
        .unwrap_or_else(|| "ANY".to_string());

    let detail = format!(".{} is {}", name, type_str);
    out.symbols.push(Symbol {
        name,
        kind: SymbolKind::Member,
        detail: detail.clone(),
        documentation: format!("**Object member field.**\n\n```pml\nmember {}\n```", detail),
        start_line: node.start_position().row,
    });
}

fn extract_assignment(node: Node, src: &[u8], out: &mut SymbolTable) {
    // Only capture assignments to GLOBAL variables (!!something).
    // Skip simple !local assignments to keep the namespace clean.
    let Some(var_node) = child_by_kind(node, "variable") else {
        return;
    };
    let name = node_text(var_node, src);
    if !name.starts_with("!!") {
        return;
    }

    let detail = format!("{} (global variable)", name);
    out.symbols.push(Symbol {
        name,
        kind: SymbolKind::Variable,
        detail: detail.clone(),
        documentation: format!("**Global variable** assigned in this file.\n\n```pml\n{}\n```", detail),
        start_line: node.start_position().row,
    });
}

/// Build a comma-separated parameter list like "!a is REAL, !b is STRING".
fn format_params(def_node: Node, src: &[u8]) -> String {
    let Some(plist) = child_by_kind(def_node, "parameter_list") else {
        return String::new();
    };
    let params: Vec<String> = collect_children(plist, &["parameter"])
        .iter()
        .map(|p| {
            let pname = child_by_kind(*p, "variable").map(|n| node_text(n, src))
                .unwrap_or_else(|| "?".to_string());
            let ptype = child_by_kind(*p, "type_identifier").map(|n| node_text(n, src))
                .unwrap_or_else(|| "ANY".to_string());
            format!("{} is {}", pname, ptype)
        })
        .collect();
    params.join(", ")
}

/// Returns the return-type clause " is RETURNTYPE" or empty string.
/// Grammar pattern: after the parameter_list, a `keyword_is` followed by
/// an `identifier` or `type_identifier`.
fn format_return_type(def_node: Node, src: &[u8]) -> String {
    // Look for a `keyword_is` that is NOT inside a parameter_list, followed
    // by an identifier/type_identifier.
    let mut cursor = def_node.walk();
    let mut saw_keyword_is = false;
    let mut saw_param_list = false;
    for child in def_node.named_children(&mut cursor) {
        match child.kind() {
            "parameter_list" => saw_param_list = true,
            "keyword_is" if saw_param_list => saw_keyword_is = true,
            "identifier" | "type_identifier" if saw_keyword_is => {
                return format!(" is {}", node_text(child, src));
            }
            _ => {}
        }
    }
    String::new()
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
    fn extracts_function_definition() {
        let table = extract_from(
            "define function !!Area(!Length is REAL, !Width is REAL) is REAL\n\
            !Area = !Length * !Width\n\
            return !Area\n\
            endfunction\n",
        );
        let func = table.symbols.iter().find(|s| s.name == "!!Area")
            .expect("function not extracted");
        assert!(matches!(func.kind, SymbolKind::Function));
        assert!(func.detail.contains("REAL"));
    }

    #[test]
    fn skips_local_assignments_inside_function() {
        let table = extract_from(
            "define function !!Foo() is REAL\n\
            !x = 1\n\
            return !x\n\
            endfunction\n",
        );
        // !x is a local — should NOT appear
        assert!(!table.symbols.iter().any(|s| s.name == "!x"));
    }

    #[test]
    fn extracts_global_assignment() {
        let table = extract_from("!!myGlobal = 'hello'\n");
        let g = table.symbols.iter().find(|s| s.name == "!!myGlobal")
            .expect("global not extracted");
        assert!(matches!(g.kind, SymbolKind::Variable));
    }
}
