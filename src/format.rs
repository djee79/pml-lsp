//! Formatting: re-indent lines according to the block structure in the parse
//! tree, and strip trailing whitespace. Nothing else is touched: PML code is
//! case-insensitive and often deliberately spaced, so content stays as is.

use tower_lsp::lsp_types::{Position, Range, TextEdit};
use tree_sitter::{Node, Tree};

/// Node kinds that open an indented block, with the keyword that closes them.
const BLOCKS: &[(&str, &str)] = &[
    ("function_definition", "keyword_endfunction"),
    ("method_definition", "keyword_endmethod"),
    ("object_definition", "keyword_endobject"),
    ("form_definition", "keyword_exit"),
    ("frame_block", "keyword_exit"),
    ("if_statement", "keyword_endif"),
    ("do_loop", "keyword_enddo"),
    ("handle_statement", "keyword_endhandle"),
];

/// Keywords inside a block that sit at the block's own level.
const MIDDLES: &[&str] = &["keyword_else", "keyword_elseif", "keyword_elsehandle"];

/// Indentation level of every line (0-based rows). Continuation lines (after
/// a trailing `$`) get two extra levels.
pub fn levels(tree: &Tree, text: &str) -> Vec<i32> {
    let line_count = text.lines().count();
    let mut levels = vec![0i32; line_count];
    walk(tree.root_node(), &mut levels);

    let lines: Vec<&str> = text.lines().collect();
    let mut prev_continued = false;
    for (i, line) in lines.iter().enumerate() {
        if prev_continued {
            levels[i] += 2;
        }
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            prev_continued = trimmed.ends_with('$') && !trimmed.ends_with("$$");
        }
    }
    levels.iter_mut().for_each(|l| *l = (*l).max(0));
    levels
}

fn walk(node: Node, levels: &mut [i32]) {
    if let Some((_, closer)) = BLOCKS.iter().find(|(kind, _)| *kind == node.kind()) {
        let open_row = node.start_position().row;
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        let close_row = children
            .iter()
            .rev()
            .find(|c| c.kind() == *closer)
            .map(|c| c.start_position().row)
            .unwrap_or_else(|| {
                // Unterminated block (error recovery): indent to the end.
                let end = node.end_position();
                if end.column == 0 { end.row } else { end.row + 1 }
            });
        for row in open_row + 1..close_row.min(levels.len()) {
            levels[row] += 1;
        }
        for c in &children {
            if MIDDLES.contains(&c.kind()) {
                let row = c.start_position().row;
                if row > open_row && row < close_row && row < levels.len() {
                    levels[row] -= 1;
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, levels);
    }
}

/// Minimal edits that bring `text` to the computed indentation and remove
/// trailing whitespace. `indent` is one level (e.g. two spaces or a tab).
pub fn edits(tree: &Tree, text: &str, indent: &str, only_lines: Option<(u32, u32)>) -> Vec<TextEdit> {
    let levels = levels(tree, text);
    let mut out = Vec::new();
    for (row, line) in text.lines().enumerate() {
        if let Some((a, b)) = only_lines {
            if (row as u32) < a || (row as u32) > b {
                continue;
            }
        }
        let chars: Vec<char> = line.chars().collect();
        let leading = chars.iter().take_while(|c| c.is_whitespace()).count();
        let content_end = chars.len() - chars.iter().rev().take_while(|c| c.is_whitespace()).count();
        let row = row as u32;

        if content_end <= leading {
            // Blank line: remove all whitespace.
            if !chars.is_empty() {
                out.push(TextEdit::new(Range::new(Position::new(row, 0), Position::new(row, chars.len() as u32)), String::new()));
            }
            continue;
        }
        let wanted = indent.repeat(levels[row as usize] as usize);
        let current: String = chars[..leading].iter().collect();
        if current != wanted {
            out.push(TextEdit::new(Range::new(Position::new(row, 0), Position::new(row, leading as u32)), wanted));
        }
        if content_end < chars.len() {
            out.push(TextEdit::new(
                Range::new(Position::new(row, content_end as u32), Position::new(row, chars.len() as u32)),
                String::new(),
            ));
        }
    }
    out
}

/// Apply edits (same line, non-overlapping) to produce the formatted text.
#[cfg(test)]
pub fn apply(text: &str, edits: &[TextEdit]) -> String {
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.range.start.line, std::cmp::Reverse(e.range.start.character)));
    for e in sorted {
        let row = e.range.start.line as usize;
        let chars: Vec<char> = lines[row].chars().collect();
        let (a, b) = (e.range.start.character as usize, e.range.end.character as usize);
        let mut s: String = chars[..a].iter().collect();
        s.push_str(&e.new_text);
        s.extend(chars[b.min(chars.len())..].iter());
        lines[row] = s;
    }
    let mut s = lines.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::PmlParser;

    fn fmt(src: &str) -> String {
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        apply(src, &edits(&tree, src, "  ", None))
    }

    #[test]
    fn indents_nested_blocks() {
        let src = "define method .m(!a is REAL)\n!x = 1\nif (!a gt 1) then\n!x = 2\nelseif (!a eq 0) then\n!x = 3\nelse\ndo !i from 1 to 3\n!x = !x + !i\nenddo\nendif\nhandle any\n$P oops\nelsehandle none\nreturn\nendhandle\nendmethod\n";
        let want = "define method .m(!a is REAL)\n  !x = 1\n  if (!a gt 1) then\n    !x = 2\n  elseif (!a eq 0) then\n    !x = 3\n  else\n    do !i from 1 to 3\n      !x = !x + !i\n    enddo\n  endif\n  handle any\n    $P oops\n  elsehandle none\n    return\n  endhandle\nendmethod\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn indents_forms_frames_and_objects() {
        let src = "setup form !!f dialog\ntitle |x|\nframe .fr |F|\nbutton .ok |OK|\nexit\nexit\ndefine object BOX\nmember .w is REAL\nendobject\n";
        let want = "setup form !!f dialog\n  title |x|\n  frame .fr |F|\n    button .ok |OK|\n  exit\nexit\ndefine object BOX\n  member .w is REAL\nendobject\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn continuation_lines_blank_lines_and_trailing_spaces() {
        let src = "define method .m()\n    !x = !!f(1, $\n!y)   \n\n   \n  -- comment  \nendmethod\n";
        let want = "define method .m()\n  !x = !!f(1, $\n      !y)\n\n\n  -- comment\nendmethod\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn already_formatted_text_needs_no_edits() {
        let src = "define method .m()\n  if (!a) then\n    !x = 1\n  endif\nendmethod\n";
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        assert!(edits(&tree, src, "  ", None).is_empty());
    }

    #[test]
    fn range_limits_edits() {
        let src = "define method .m()\n!x = 1\n!y = 2\nendmethod\n";
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        let e = edits(&tree, src, "  ", Some((2, 2)));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].range.start.line, 2);
    }

    #[test]
    fn unterminated_block_is_left_alone() {
        // Error recovery yields no block node, so nothing is re-indented.
        let src = "if (!a) then\n!x = 1\n!y = 2\n";
        assert_eq!(fmt(src), src);
    }
}
