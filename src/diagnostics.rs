//! Syntax diagnostics derived from the tree-sitter parse tree.
//!
//! The PML grammar is line-oriented, so an `ERROR` node never spans more than
//! the line it starts on. `MISSING` nodes are zero-width tokens tree-sitter
//! inserted to recover; the most common one is a missing newline, which means
//! the parser gave up on the rest of the line.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use tree_sitter::{Node, Tree};

pub fn collect(tree: &Tree, source: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if tree.root_node().has_error() {
        visit(tree.root_node(), source.as_bytes(), &mut out);
    }
    out
}

fn visit(node: Node, src: &[u8], out: &mut Vec<Diagnostic>) {
    if node.is_error() {
        out.push(diagnostic(node, src, error_message(node, src)));
        // Children of an ERROR node are the skipped tokens; one report is enough.
        return;
    }
    if node.is_missing() {
        out.push(diagnostic(node, src, missing_message(node)));
        return;
    }
    if !node.has_error() {
        return;
    }
    let mut cursor = node.walk();
    let mut child_reported = false;
    for child in node.children(&mut cursor) {
        if child.has_error() || child.is_error() || child.is_missing() {
            child_reported = true;
        }
        visit(child, src, out);
    }
    // The error is inside this node but no visible child carries it: the
    // parser inserted a hidden missing token, which for this grammar is the
    // `_newline` that should have ended the statement. Hidden nodes are not
    // exposed through the node API, so report it from the parent.
    if !child_reported {
        out.push(diagnostic_at_end(node, src, "Unexpected text at end of statement".to_string()));
    }
}

/// Diagnostic from the end of `node` to the end of that line.
fn diagnostic_at_end(node: Node, src: &[u8], message: String) -> Diagnostic {
    let end = node.end_position();
    let line_len = src
        .split(|&b| b == b'\n')
        .nth(end.row)
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l).len())
        .unwrap_or(end.column);
    Diagnostic {
        range: Range::new(
            Position::new(end.row as u32, end.column as u32),
            Position::new(end.row as u32, line_len.max(end.column + 1) as u32),
        ),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("pml-lsp".to_string()),
        message,
        ..Default::default()
    }
}

fn error_message(node: Node, src: &[u8]) -> String {
    let text = node.utf8_text(src).unwrap_or("").trim();
    let shown: String = text.chars().take(40).collect();
    if shown.is_empty() {
        "Syntax error".to_string()
    } else if shown.len() < text.len() {
        format!("Syntax error near `{}…`", shown)
    } else {
        format!("Syntax error near `{}`", shown)
    }
}

fn missing_message(node: Node) -> String {
    match node.kind() {
        "_newline" => "Unexpected text at end of statement".to_string(),
        kind => format!("Missing `{}`", kind.trim_start_matches("keyword_")),
    }
}

fn diagnostic(node: Node, src: &[u8], message: String) -> Diagnostic {
    let start = node.start_position();
    let mut end = node.end_position();
    if node.is_missing() || (start.row == end.row && start.column == end.column) {
        // Zero-width: highlight to the end of the line so it is visible.
        let line_len = src
            .split(|&b| b == b'\n')
            .nth(start.row)
            .map(|l| l.len())
            .unwrap_or(start.column);
        end.column = line_len.max(start.column + 1);
    }
    Diagnostic {
        range: Range::new(
            Position::new(start.row as u32, start.column as u32),
            Position::new(end.row as u32, end.column as u32),
        ),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("pml-lsp".to_string()),
        message,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::PmlParser;

    fn diags(src: &str) -> Vec<Diagnostic> {
        let parser = PmlParser::new();
        collect(&parser.parse(src).unwrap(), src)
    }

    #[test]
    fn clean_file_has_no_diagnostics() {
        assert!(diags("define method .m()\n  !x = object STRING()\nendmethod\n").is_empty());
    }

    #[test]
    fn error_is_confined_to_its_line() {
        let d = diags("!x = = 1\n!y = 2\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].range.start.line, 0);
        assert_eq!(d[0].range.end.line, 0);
        assert!(d[0].message.starts_with("Syntax error"));
    }

    #[test]
    fn trailing_junk_reports_missing_newline() {
        let d = diags("!x = !a wrt /*\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "Unexpected text at end of statement");
        assert_eq!(d[0].range.start, Position::new(0, 7));
        assert_eq!(d[0].range.end, Position::new(0, 14));
    }

    #[test]
    fn trailing_junk_inside_a_block_is_reported_once() {
        let d = diags("define method .m()\n  !y = !a wrt /*\nendmethod\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].range.start.line, 1);
    }

    #[test]
    fn unterminated_block_reports_missing_keyword() {
        let d = diags("if (!x) then\n  !y = 1\n");
        assert!(!d.is_empty());
        assert!(d.iter().any(|x| x.message.contains("endif") || x.message.starts_with("Syntax error")));
    }
}
