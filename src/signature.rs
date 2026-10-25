//! Signature help: which call is the cursor inside, and which argument.

use tower_lsp::lsp_types::{Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position, SignatureInformation};

#[derive(Debug, PartialEq, Eq)]
pub struct CallSite {
    /// The callee as written: `!!Area`, `.upcase`, `STRING`, `object BOX`.
    pub callee: String,
    /// Zero-based index of the argument the cursor is in.
    pub active_parameter: u32,
}

/// Find the innermost unclosed call before `pos`. Handles `$` line
/// continuations by joining the previous lines that end with `$`.
pub fn call_at(text: &str, pos: Position) -> Option<CallSite> {
    let lines: Vec<&str> = text.lines().collect();
    let row = pos.line as usize;
    let current: String = lines.get(row)?.chars().take(pos.character as usize).collect();

    // Prepend continued lines.
    let mut logical = current;
    let mut r = row;
    while r > 0 {
        let prev = lines[r - 1].trim_end();
        if let Some(stripped) = prev.strip_suffix('$') {
            logical = format!("{} {}", stripped, logical);
            r -= 1;
        } else {
            break;
        }
    }

    let chars: Vec<char> = logical.chars().collect();
    let mut depth = 0i32;
    let mut commas = 0u32;
    let mut in_string: Option<char> = None;
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if let Some(q) = in_string {
            if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '\'' | '|' => in_string = Some(c),
            ')' | ']' => depth += 1,
            '[' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            '(' => {
                if depth == 0 {
                    let callee = callee_before(&chars[..i])?;
                    return Some(CallSite { callee, active_parameter: commas });
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The identifier-ish token ending at `chars`' end: `!!Area`, `.upcase`,
/// `STRING`, or `object BOX`.
fn callee_before(chars: &[char]) -> Option<String> {
    let mut end = chars.len();
    while end > 0 && chars[end - 1] == ' ' {
        end -= 1;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = end;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let mut prefix_start = start;
    while prefix_start > 0 && matches!(chars[prefix_start - 1], '!' | '$') {
        prefix_start -= 1;
    }
    if prefix_start == start && start > 0 && chars[start - 1] == '.' {
        prefix_start = start - 1;
    }
    let token: String = chars[prefix_start..end].iter().collect();
    // `object BOX(` -> constructor of BOX
    let before: String = chars[..prefix_start].iter().collect();
    if before.trim_end().to_ascii_lowercase().ends_with("object") && !token.starts_with(['!', '.']) {
        return Some(format!("object {}", token));
    }
    Some(token)
}

/// Build LSP signature information from a one-line signature such as
/// `define function !!Area(!Length is REAL, !Width is REAL) is REAL` or
/// `Replace(STRING old, STRING new [, REAL count]) IS STRING`.
pub fn signature_from_detail(detail: &str, documentation: &str) -> SignatureInformation {
    let label = detail
        .trim()
        .trim_start_matches("define function ")
        .trim_start_matches("define method ")
        .to_string();
    let mut parameters = Vec::new();
    if let (Some(open), Some(close)) = (label.find('('), label.rfind(')')) {
        if close > open {
            let inner = &label[open + 1..close];
            // Only parentheses nest; `[, REAL count]` marks an optional
            // parameter and its comma still separates parameters.
            let mut depth = 0;
            let mut start = 0;
            for (i, c) in inner.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        push_param(&mut parameters, &label, open + 1 + start, open + 1 + i);
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            push_param(&mut parameters, &label, open + 1 + start, close);
        }
    }
    SignatureInformation {
        label,
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: documentation.to_string(),
        })),
        parameters: if parameters.is_empty() { None } else { Some(parameters) },
        active_parameter: None,
    }
}

/// Number of parameters in a definition's detail string, e.g. 2 for
/// `define method .m(!a is REAL, !b is STRING) is REAL`. `None` when the
/// detail has no parameter list.
pub fn parameter_count(detail: &str) -> Option<usize> {
    let open = detail.find('(')?;
    let mut depth = 0;
    let mut count = 0;
    let mut saw_content = false;
    for c in detail[open + 1..].chars() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(if saw_content { count + 1 } else { 0 }),
            ')' => depth -= 1,
            ',' if depth == 0 => count += 1,
            c if !c.is_whitespace() => saw_content = true,
            _ => {}
        }
    }
    None
}

fn push_param(out: &mut Vec<ParameterInformation>, label: &str, mut start: usize, mut end: usize) {
    let trim = |b: u8| matches!(b, b' ' | b'[' | b']');
    while start < end && trim(label.as_bytes()[start]) {
        start += 1;
    }
    while end > start && trim(label.as_bytes()[end - 1]) {
        end -= 1;
    }
    if start < end {
        out.push(ParameterInformation {
            label: ParameterLabel::LabelOffsets([start as u32, end as u32]),
            documentation: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(line: &str) -> Option<CallSite> {
        call_at(line, Position::new(0, line.chars().count() as u32))
    }

    #[test]
    fn finds_callee_and_argument_index() {
        assert_eq!(call("!x = !!Area(1, "), Some(CallSite { callee: "!!Area".into(), active_parameter: 1 }));
        assert_eq!(call("!s.Replace('a', 'b', "), Some(CallSite { callee: ".Replace".into(), active_parameter: 2 }));
        assert_eq!(call("!x = STRING("), Some(CallSite { callee: "STRING".into(), active_parameter: 0 }));
        assert_eq!(call("!p = object POSITION(1, "), Some(CallSite { callee: "object POSITION".into(), active_parameter: 1 }));
    }

    #[test]
    fn ignores_commas_in_nested_calls_and_strings() {
        assert_eq!(call("!!f(!a.b(1, 2), "), Some(CallSite { callee: "!!f".into(), active_parameter: 1 }));
        assert_eq!(call("!!f('x, y', |a, b|, "), Some(CallSite { callee: "!!f".into(), active_parameter: 2 }));
        assert_eq!(call("!!f(!arr[1, 2], "), Some(CallSite { callee: "!!f".into(), active_parameter: 1 }));
    }

    #[test]
    fn none_outside_a_call() {
        assert_eq!(call("!x = 1 + 2"), None);
        assert_eq!(call("!x = !!f(1)"), None);
    }

    #[test]
    fn follows_line_continuation() {
        let text = "!x = !!Area(1, $\n          ";
        assert_eq!(call_at(text, Position::new(1, 10)), Some(CallSite { callee: "!!Area".into(), active_parameter: 1 }));
    }

    #[test]
    fn counts_parameters() {
        assert_eq!(parameter_count("define method .m() is REAL"), Some(0));
        assert_eq!(parameter_count("define method .m(!a is REAL)"), Some(1));
        assert_eq!(parameter_count("define function !!f(!a is REAL, !b is ARRAY) is BOOLEAN"), Some(2));
        assert_eq!(parameter_count("!!x"), None);
    }

    #[test]
    fn parses_parameters_from_detail() {
        let s = signature_from_detail("define function !!Area(!Length is REAL, !Width is REAL) is REAL", "doc");
        assert_eq!(s.label, "!!Area(!Length is REAL, !Width is REAL) is REAL");
        let p = s.parameters.unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].label, ParameterLabel::LabelOffsets([7, 22]));
        assert_eq!(&s.label[7..22], "!Length is REAL");
        let b = signature_from_detail("Replace(STRING old, STRING new [, REAL count]) IS STRING", "");
        let bp = b.parameters.unwrap();
        assert_eq!(bp.len(), 3);
        let ParameterLabel::LabelOffsets([a, z]) = bp[2].label else { panic!() };
        assert_eq!(&b.label[a as usize..z as usize], "REAL count");
        assert!(signature_from_detail("Length() IS REAL", "").parameters.is_none());
    }
}
