//! Formatting: re-indent lines according to the block structure in the parse
//! tree, strip trailing whitespace and, when asked, normalise keyword case.
//! Nothing else is touched: PML code is case-insensitive and often
//! deliberately spaced, so content stays as is.

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
const MIDDLES: &[&str] = &["keyword_else", "keyword_elsehandle", "keyword_elseif"];

/// How keywords (`define`, `if`, `endif`, `eq`, ...) should be spelled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeywordCase {
    /// Leave them as written.
    #[default]
    Keep,
    Lower,
    Upper,
}

impl KeywordCase {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "keep" | "preserve" | "none" => Some(KeywordCase::Keep),
            "lower" | "lowercase" => Some(KeywordCase::Lower),
            "upper" | "uppercase" => Some(KeywordCase::Upper),
            _ => None,
        }
    }

    fn apply(self, word: &str) -> String {
        match self {
            KeywordCase::Keep => word.to_string(),
            KeywordCase::Lower => word.to_ascii_lowercase(),
            KeywordCase::Upper => word.to_ascii_uppercase(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// One indentation level, e.g. two spaces or a tab.
    pub indent: String,
    pub keyword_case: KeywordCase,
}

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

fn in_lines(row: u32, only_lines: Option<(u32, u32)>) -> bool {
    only_lines.map(|(a, b)| row >= a && row <= b).unwrap_or(true)
}

/// Minimal edits that bring `text` to the computed indentation, remove
/// trailing whitespace and apply the requested keyword case.
pub fn edits(tree: &Tree, text: &str, options: &Options, only_lines: Option<(u32, u32)>) -> Vec<TextEdit> {
    let levels = levels(tree, text);
    let mut out = Vec::new();
    for (row, line) in text.lines().enumerate() {
        if !in_lines(row as u32, only_lines) {
            continue;
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
        let wanted = options.indent.repeat(levels[row as usize] as usize);
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
    out.extend(keyword_edits(tree, text, options.keyword_case, only_lines));
    out
}

/// Edits that respell every keyword token in `case`. Keywords are single
/// tokens on one line, so each edit stays inside its line and never touches
/// the leading or trailing whitespace that [`edits`] rewrites.
pub fn keyword_edits(tree: &Tree, text: &str, case: KeywordCase, only_lines: Option<(u32, u32)>) -> Vec<TextEdit> {
    let mut out = Vec::new();
    if case == KeywordCase::Keep {
        return out;
    }
    let lines: Vec<&str> = text.lines().collect();
    let src = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind().starts_with("keyword_") {
            let row = node.start_position().row;
            let Some(line) = lines.get(row) else { continue };
            if !in_lines(row as u32, only_lines) {
                continue;
            }
            let word = node.utf8_text(src).unwrap_or("");
            let wanted = case.apply(word);
            if wanted != word {
                let start = char_col(line, node.start_position().column);
                let end = char_col(line, node.end_position().column);
                out.push(TextEdit::new(
                    Range::new(Position::new(row as u32, start as u32), Position::new(row as u32, end as u32)),
                    wanted,
                ));
            }
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|e| (e.range.start.line, e.range.start.character));
    out
}

/// Tree-sitter columns are byte offsets; LSP positions count characters.
pub fn char_col(line: &str, byte_col: usize) -> usize {
    line.get(..byte_col).map(|s| s.chars().count()).unwrap_or(byte_col)
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

    fn opts(case: KeywordCase) -> Options {
        Options { indent: "  ".into(), keyword_case: case }
    }

    fn fmt_with(src: &str, case: KeywordCase) -> String {
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        apply(src, &edits(&tree, src, &opts(case), None))
    }

    fn fmt(src: &str) -> String {
        fmt_with(src, KeywordCase::Keep)
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
        assert!(edits(&tree, src, &opts(KeywordCase::Keep), None).is_empty());
        assert!(edits(&tree, src, &opts(KeywordCase::Lower), None).is_empty());
    }

    #[test]
    fn range_limits_edits() {
        let src = "define method .m()\n!x = 1\n!y = 2\nendmethod\n";
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        let e = edits(&tree, src, &opts(KeywordCase::Keep), Some((2, 2)));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].range.start.line, 2);
    }

    #[test]
    fn unterminated_block_is_left_alone() {
        // Error recovery yields no block node, so nothing is re-indented.
        let src = "if (!a) then\n!x = 1\n!y = 2\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn keyword_case_lower_and_upper() {
        let src = "DEFINE METHOD .m(!a IS REAL)\n  IF (!a GT 1 And !a Ne 2) Then\n    !x = Object STRING()\n  EndIf\nENDMETHOD\n";
        let lower = "define method .m(!a is REAL)\n  if (!a gt 1 and !a ne 2) then\n    !x = object STRING()\n  endif\nendmethod\n";
        let upper = "DEFINE METHOD .m(!a IS REAL)\n  IF (!a GT 1 AND !a NE 2) THEN\n    !x = OBJECT STRING()\n  ENDIF\nENDMETHOD\n";
        assert_eq!(fmt_with(src, KeywordCase::Lower), lower);
        assert_eq!(fmt_with(src, KeywordCase::Upper), upper);
        // Keep leaves the text alone (it is already indented).
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn keyword_case_leaves_commands_strings_and_names_alone() {
        // `to` and `from` inside a command are plain words, `|IF|` is a string,
        // `!to` is a variable and `STRING` is a type name.
        let src = "define method .m()\n  q var !x to from\n  !to = |IF then|\n  !s = object STRING()\nendmethod\n";
        let want = "DEFINE METHOD .m()\n  q var !x to from\n  !to = |IF then|\n  !s = OBJECT STRING()\nENDMETHOD\n";
        assert_eq!(fmt_with(src, KeywordCase::Upper), want);
    }

    #[test]
    fn keyword_edits_use_character_columns() {
        let src = "if (!a eq |é|) THEN\nendif\n";
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        let e = keyword_edits(&tree, src, KeywordCase::Lower, None);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].range.start, Position::new(0, 15));
        assert_eq!(e[0].range.end, Position::new(0, 19));
        assert_eq!(apply(src, &e), "if (!a eq |é|) then\nendif\n");
    }

    #[test]
    fn parses_setting_values() {
        assert_eq!(KeywordCase::parse("lower"), Some(KeywordCase::Lower));
        assert_eq!(KeywordCase::parse("UPPERCASE"), Some(KeywordCase::Upper));
        assert_eq!(KeywordCase::parse("keep"), Some(KeywordCase::Keep));
        assert_eq!(KeywordCase::parse("camel"), None);
    }
}
