//! Code actions: method stubs and error handlers.

use tower_lsp::lsp_types::{CodeAction, CodeActionKind, Position, Range, TextEdit, Url, WorkspaceEdit};
use std::collections::HashMap;

fn single_edit(uri: &Url, title: &str, kind: CodeActionKind, edit: TextEdit) -> CodeAction {
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![edit]);
    CodeAction {
        title: title.to_string(),
        kind: Some(kind),
        edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
        ..Default::default()
    }
}

/// `define method .name(...) ... endmethod` appended to the file, with one
/// parameter per argument of the call at `line` (named after `!var` arguments
/// when possible).
pub fn create_method(uri: &Url, text: &str, method: &str, line: &str, indent: &str) -> CodeAction {
    let name = method.trim_start_matches('.');
    let params = call_arguments(line, name)
        .into_iter()
        .enumerate()
        .map(|(i, arg)| {
            let var = arg.trim();
            let pname = if var.starts_with('!') && var.chars().skip(1).all(|c| c.is_alphanumeric() || c == '_') && !var.starts_with("!!") {
                var.to_string()
            } else {
                format!("!arg{}", i + 1)
            };
            format!("{} is ANY", pname)
        })
        .collect::<Vec<_>>()
        .join(", ");

    let line_count = text.lines().count() as u32;
    let mut stub = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        stub.push('\n');
    }
    stub.push_str(&format!("\ndefine method .{}({})\n{}\nendmethod\n", name, params, indent));
    let at = Position::new(line_count, 0);
    single_edit(
        uri,
        &format!("Create method .{}()", name),
        CodeActionKind::QUICKFIX,
        TextEdit::new(Range::new(at, at), stub),
    )
}

/// Arguments of the call `.name(...)` found in `line`, split at top-level commas.
fn call_arguments(line: &str, name: &str) -> Vec<String> {
    let lower = line.to_ascii_lowercase();
    let needle = format!(".{}(", name.to_ascii_lowercase());
    let Some(i) = lower.find(&needle) else { return Vec::new() };
    let rest: Vec<char> = line[i + needle.len()..].chars().collect();
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    for c in rest {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '|' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                cur.push(c);
            }
            ')' => break,
            ',' if depth == 0 => {
                args.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        args.push(cur);
    }
    args
}

/// `handle any ... endhandle` inserted after `row`, at that line's indentation.
pub fn add_handle(uri: &Url, line: &str, row: u32, indent: &str) -> CodeAction {
    let lead: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let block = format!(
        "{lead}handle any\n{lead}{indent}$P $!!error.text\n{lead}endhandle\n",
        lead = lead,
        indent = indent
    );
    let at = Position::new(row + 1, 0);
    single_edit(uri, "Add handle any … endhandle after this statement", CodeActionKind::REFACTOR, TextEdit::new(Range::new(at, at), block))
}

/// Is this line a plain statement that a `handle` block can follow?
pub fn is_handleable(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with("--") || t.starts_with("$*") {
        return false;
    }
    let first = t.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    !matches!(
        first.as_str(),
        "define" | "if" | "elseif" | "else" | "endif" | "do" | "enddo" | "handle" | "elsehandle" | "endhandle"
            | "endmethod" | "endfunction" | "endobject" | "setup" | "layout" | "exit" | "frame" | "member" | "return"
    )
}

/// Reformat the whole file as a source action.
pub fn format_document(uri: &Url, edits: Vec<TextEdit>) -> Option<CodeAction> {
    source_action(uri, "Format document", CodeActionKind::SOURCE_FIX_ALL, edits)
}

/// A whole-file action made of `edits`; none when there is nothing to change.
pub fn source_action(uri: &Url, title: &str, kind: CodeActionKind, edits: Vec<TextEdit>) -> Option<CodeAction> {
    if edits.is_empty() {
        return None;
    }
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(CodeAction {
        title: title.to_string(),
        kind: Some(kind),
        edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri() -> Url {
        Url::parse("file:///tmp/x.pmlfrm").unwrap()
    }

    fn edit_text(a: &CodeAction) -> (Position, String) {
        let e = &a.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri()][0];
        (e.range.start, e.new_text.clone())
    }

    #[test]
    fn method_stub_from_call_arguments() {
        let text = "define method .ok()\n  !this.resize(!w, 2, !a.b(1,2))\nendmethod\n";
        let a = create_method(&uri(), text, ".resize", "  !this.resize(!w, 2, !a.b(1,2))", "  ");
        assert_eq!(a.title, "Create method .resize()");
        let (pos, s) = edit_text(&a);
        assert_eq!(pos, Position::new(3, 0));
        assert_eq!(s, "\ndefine method .resize(!w is ANY, !arg2 is ANY, !arg3 is ANY)\n  \nendmethod\n");
    }

    #[test]
    fn method_stub_without_trailing_newline_adds_one() {
        let (_, s) = edit_text(&create_method(&uri(), "!this.go()", ".go", "!this.go()", "  "));
        assert!(s.starts_with("\n\ndefine method .go()\n"));
    }

    #[test]
    fn handle_block_follows_indentation() {
        let a = add_handle(&uri(), "    !x = !!risky()", 4, "  ");
        let (pos, s) = edit_text(&a);
        assert_eq!(pos, Position::new(5, 0));
        assert_eq!(s, "    handle any\n      $P $!!error.text\n    endhandle\n");
    }

    #[test]
    fn handleable_lines() {
        assert!(is_handleable("  !x = !!risky()"));
        assert!(is_handleable("  !this.save()"));
        assert!(is_handleable("  q var !x"));
        assert!(!is_handleable("  if (!x) then"));
        assert!(!is_handleable("  -- comment"));
        assert!(!is_handleable("endmethod"));
        assert!(!is_handleable(""));
    }
}
