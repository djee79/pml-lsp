//! What is the user typing? Decides which completion items make sense and
//! which text the chosen item should replace.

use std::collections::BTreeSet;
use tower_lsp::lsp_types::{Position, Range};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    /// Right after `.`: methods, members, gadgets. `receiver` is the text
    /// before the dot, e.g. `!this`, `!!myForm.input`, `!str`.
    Member { receiver: String },
    /// Typing a variable: `!` (local) or `!!` / `$!!` (global).
    Variable { global: bool },
    /// Anything else: keywords, commands, snippets, constructors, functions.
    Statement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub context: Context,
    /// The text a completion should replace: the partial word being typed,
    /// including its `!`/`!!`/`$!` prefix for variables.
    pub replace: Range,
}

/// Classify the cursor position from the text of the line before it.
pub fn at(text: &str, pos: Position) -> Cursor {
    let line = text.lines().nth(pos.line as usize).unwrap_or("");
    let before: Vec<char> = line.chars().take(pos.character as usize).collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    // Start of the partial word being typed.
    let mut word_start = before.len();
    while word_start > 0 && is_word(before[word_start - 1]) {
        word_start -= 1;
    }
    let head: String = before[..word_start].iter().collect();

    let (context, replace_start) = if let Some(receiver_part) = head.strip_suffix('.') {
        let receiver: String = receiver_part
            .chars()
            .rev()
            .take_while(|&c| is_word(c) || matches!(c, '!' | '$' | '.' | ')' | '(' | '[' | ']'))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        (Context::Member { receiver }, word_start)
    } else if head.ends_with("!!") || head.ends_with("$!!") {
        let prefix = if head.ends_with("$!!") { 3 } else { 2 };
        (Context::Variable { global: true }, word_start - prefix)
    } else if head.ends_with('!') {
        let prefix = if head.ends_with("$!") { 2 } else { 1 };
        (Context::Variable { global: false }, word_start - prefix)
    } else {
        (Context::Statement, word_start)
    };

    Cursor {
        context,
        replace: Range::new(Position::new(pos.line, replace_start as u32), pos),
    }
}

/// The word under the cursor, including a `!`/`!!`/`$!` prefix or a leading
/// `.`, so that `!this.Name` yields `.Name` and `!!Func` is returned whole.
pub fn word_at(text: &str, pos: Position) -> Option<String> {
    word_span_at(text, pos).map(|(w, _)| w)
}

/// Like [`word_at`], also returning the range of the bare name (without the
/// prefix), which is what a rename replaces.
pub fn word_span_at(text: &str, pos: Position) -> Option<(String, Range)> {
    let line = text.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let mut col = (pos.character as usize).min(chars.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    // Cursor on the `!`, `$` or `.` prefix: move onto the name it introduces.
    while col < chars.len() && matches!(chars[col], '!' | '$' | '.') {
        col += 1;
    }

    let mut start = col;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
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
    let word: String = chars[prefix_start..end].iter().collect();
    let range = Range::new(Position::new(pos.line, start as u32), Position::new(pos.line, end as u32));
    Some((word, range))
}

/// Every distinct `!name` / `!!name` written in the document, in order of
/// first appearance. Used to complete local variables without type analysis.
pub fn variables_in(text: &str) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '!' {
            let start = i;
            while i < chars.len() && chars[i] == '!' {
                i += 1;
            }
            let name_start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if i > name_start && i - start <= name_start - start + 64 && name_start - start <= 2 {
                let name: String = chars[start..i].iter().collect();
                if seen.insert(name.to_ascii_lowercase()) {
                    out.push(name);
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// How `!var` is declared in `text`: a typed parameter (`!var is TYPE`) or
/// the right-hand side of its first assignment (`!var = <expr>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decl {
    Type(String),
    Expr(String),
}

pub fn declaration_of(text: &str, var: &str) -> Option<Decl> {
    let var_lc = var.trim_start_matches('$').to_ascii_lowercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    for line in text.lines() {
        let lc = line.to_ascii_lowercase();
        let mut from = 0;
        while let Some(i) = lc[from..].find(&var_lc) {
            let start = from + i;
            let end = start + var_lc.len();
            from = end;
            // Whole-token match only (`!a` must not match `!ab` or `!!a`).
            let before_ok = start == 0 || !matches!(lc.as_bytes()[start - 1], b'!' | b'$');
            let after_ok = end == lc.len() || !is_word(lc[end..].chars().next().unwrap());
            if !before_ok || !after_ok {
                continue;
            }
            let rest = &line[end..];
            let rest_trim = rest.trim_start();
            if rest_trim.len() >= 3 && rest_trim[..3].eq_ignore_ascii_case("is ") {
                let t: String = rest_trim[3..].trim_start().chars().take_while(|&c| is_word(c)).collect();
                if !t.is_empty() {
                    return Some(Decl::Type(t.to_ascii_uppercase()));
                }
            }
            if let Some(rhs) = rest_trim.strip_prefix('=').filter(|r| !r.starts_with('=')) {
                return Some(Decl::Expr(strip_comment(rhs).trim().to_string()));
            }
        }
    }
    None
}

/// Remove a trailing `--` or `$*` comment that is not inside a string.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let bytes: Vec<(usize, char)> = line.char_indices().collect();
    for (k, (i, c)) in bytes.iter().enumerate() {
        match quote {
            Some(q) if *c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '|' => quote = Some(*c),
                '-' if bytes.get(k + 1).map(|(_, n)| *n == '-').unwrap_or(false) => return &line[..*i],
                '$' if bytes.get(k + 1).map(|(_, n)| *n == '*').unwrap_or(false) => return &line[..*i],
                _ => {}
            },
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(before: &str) -> Context {
        at(before, Position::new(0, before.chars().count() as u32)).context
    }

    fn replace_start(before: &str) -> u32 {
        at(before, Position::new(0, before.chars().count() as u32)).replace.start.character
    }

    #[test]
    fn member_after_dot() {
        assert_eq!(classify("  !this."), Context::Member { receiver: "!this".into() });
        assert_eq!(classify("!x = !str.up"), Context::Member { receiver: "!str".into() });
        assert_eq!(
            classify("!!myForm.input."),
            Context::Member { receiver: "!!myForm.input".into() }
        );
        assert_eq!(classify("!a.b().c"), Context::Member { receiver: "!a.b()".into() });
        // Replacement starts after the dot.
        assert_eq!(replace_start("!x = !str.up"), 10);
    }

    #[test]
    fn variables() {
        assert_eq!(classify("!x = !"), Context::Variable { global: false });
        assert_eq!(classify("!x = !!my"), Context::Variable { global: true });
        assert_eq!(classify("$P value $!v"), Context::Variable { global: false });
        // Replacement includes the prefix.
        assert_eq!(replace_start("!x = !!my"), 5);
        assert_eq!(replace_start("$P value $!v"), 9);
    }

    #[test]
    fn statement_otherwise() {
        assert_eq!(classify(""), Context::Statement);
        assert_eq!(classify("  def"), Context::Statement);
        assert_eq!(classify("!x = STR"), Context::Statement);
        assert_eq!(replace_start("!x = STR"), 5);
    }

    #[test]
    fn word_under_cursor_keeps_prefixes() {
        let t = "!x = !this.Name.upcase()\n";
        assert_eq!(word_at(t, Position::new(0, 7)).as_deref(), Some("!this"));
        assert_eq!(word_at(t, Position::new(0, 12)).as_deref(), Some(".Name"));
        assert_eq!(word_at(t, Position::new(0, 18)).as_deref(), Some(".upcase"));
        assert_eq!(word_at("!!Area(1)\n", Position::new(0, 3)).as_deref(), Some("!!Area"));
        assert_eq!(word_at("!x = $!!g\n", Position::new(0, 8)).as_deref(), Some("$!!g"));
        assert_eq!(word_at("   \n", Position::new(0, 1)), None);
        // Cursor on the prefix still finds the name.
        assert_eq!(word_at(t, Position::new(0, 5)).as_deref(), Some("!this"));
        assert_eq!(word_at(t, Position::new(0, 10)).as_deref(), Some(".Name"));
        assert_eq!(word_at("!x = $!!g\n", Position::new(0, 5)).as_deref(), Some("$!!g"));
    }

    #[test]
    fn finds_declarations() {
        let t = "define method .m(!a is STRING, !ab is REAL)\n!b = object BOX() -- a box\n!!h = 1\n!c = !a.upcase() $* note\n";
        assert_eq!(declaration_of(t, "!a"), Some(Decl::Type("STRING".into())));
        assert_eq!(declaration_of(t, "!ab"), Some(Decl::Type("REAL".into())));
        assert_eq!(declaration_of(t, "!b"), Some(Decl::Expr("object BOX()".into())));
        assert_eq!(declaration_of(t, "!c"), Some(Decl::Expr("!a.upcase()".into())));
        assert_eq!(declaration_of(t, "!h"), None, "!h must not match !!h");
        assert_eq!(declaration_of(t, "!!h"), Some(Decl::Expr("1".into())));
        assert_eq!(declaration_of(t, "!zz"), None);
    }

    #[test]
    fn collects_variables_once_each() {
        let vars = variables_in("!x = 1\n!y = !x + !!G\n!X = 2\n$P $!y\n");
        assert_eq!(vars, vec!["!x", "!y", "!!G"]);
    }
}
