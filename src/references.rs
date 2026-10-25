//! Textual, namespace-aware occurrences of a PML name, for find-references
//! and rename. PML is case-insensitive.

use tower_lsp::lsp_types::{Position, Range};

/// Namespace of a name, decided by its prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    /// `!!name`: global variable, function, form.
    Global,
    /// `!name`: local variable.
    Local,
    /// `.name`: method, member, gadget.
    Member,
    /// `name`: object type or command word.
    Bare,
}

pub fn namespace_of(word: &str) -> Namespace {
    let w = word.trim_start_matches('$');
    if w.starts_with("!!") {
        Namespace::Global
    } else if w.starts_with('!') {
        Namespace::Local
    } else if w.starts_with('.') {
        Namespace::Member
    } else {
        Namespace::Bare
    }
}

pub fn bare(word: &str) -> &str {
    word.trim_start_matches(['$', '!', '.'])
}

/// Ranges of the bare name part of every occurrence of `word` in `text`,
/// restricted to lines `[first_line, last_line]` when given.
pub fn find(text: &str, word: &str, lines: Option<(u32, u32)>) -> Vec<Range> {
    let ns = namespace_of(word);
    let needle = bare(word).to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();

    for (row, line) in text.lines().enumerate() {
        if let Some((a, b)) = lines {
            if (row as u32) < a || (row as u32) > b {
                continue;
            }
        }
        let chars: Vec<char> = line.chars().collect();
        let lower: Vec<char> = line.to_ascii_lowercase().chars().collect();
        let n = needle.chars().count();
        let needle_chars: Vec<char> = needle.chars().collect();
        let mut i = 0;
        while i + n <= chars.len() {
            if lower[i..i + n] != needle_chars[..] {
                i += 1;
                continue;
            }
            let after_ok = i + n == chars.len() || !is_word(chars[i + n]);
            // Count the `!` / `.` prefix right before the match.
            let mut p = i;
            while p > 0 && chars[p - 1] == '!' {
                p -= 1;
            }
            let bangs = i - p;
            let dot = bangs == 0 && p > 0 && chars[p - 1] == '.';
            // A `.` or `!` prefix separates the name from what precedes it
            // (`!this.scale`, `$!!x`); a bare name needs a non-word before it.
            let before_ok = dot || bangs > 0 || p == 0 || !is_word(chars[p - 1]);
            let ns_ok = match ns {
                Namespace::Global => bangs == 2,
                Namespace::Local => bangs == 1,
                Namespace::Member => dot,
                Namespace::Bare => bangs == 0 && !dot,
            };
            if after_ok && before_ok && ns_ok {
                out.push(Range::new(
                    Position::new(row as u32, i as u32),
                    Position::new(row as u32, (i + n) as u32),
                ));
            }
            i += n;
        }
    }
    out
}

/// The new bare name from what the user typed for a rename (`!!New`, `.New`
/// or `New` all give `New`). `None` if it is not a valid PML identifier.
pub fn new_bare_name(new_name: &str) -> Option<String> {
    let b = bare(new_name);
    let mut chars = b.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(b.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(text: &str, word: &str) -> Vec<(u32, u32)> {
        find(text, word, None).iter().map(|r| (r.start.line, r.start.character)).collect()
    }

    #[test]
    fn globals_do_not_match_locals_or_members() {
        let t = "!!Area = 1\n!Area = 2\n!x = !!area(3)\n!y = !o.Area\n$!!Area\n";
        assert_eq!(cols(t, "!!Area"), vec![(0, 2), (2, 7), (4, 3)]);
        assert_eq!(cols(t, "!Area"), vec![(1, 1)]);
        assert_eq!(cols(t, ".Area"), vec![(3, 8)]);
    }

    #[test]
    fn members_match_methods_and_definitions() {
        let t = "define method .scale(!f is REAL)\n  !this.scale(1)\n  !b.Scale(2)\n  !scaled = 1\n  !x.scaler()\n";
        assert_eq!(cols(t, ".scale"), vec![(0, 15), (1, 8), (2, 5)]);
    }

    #[test]
    fn bare_names_match_whole_words_only() {
        let t = "define object BOX\n!b = object box()\n!c is BOX\n!!BOX = 1\n!boxes = 2\n";
        assert_eq!(cols(t, "BOX"), vec![(0, 14), (1, 12), (2, 6)]);
    }

    #[test]
    fn line_window_restricts_matches() {
        let t = "!x = 1\n!x = 2\n!x = 3\n";
        assert_eq!(cols(t, "!x").len(), 3);
        assert_eq!(find(t, "!x", Some((1, 1))).len(), 1);
    }

    #[test]
    fn validates_new_names() {
        assert_eq!(new_bare_name("!!New").as_deref(), Some("New"));
        assert_eq!(new_bare_name(".ok2").as_deref(), Some("ok2"));
        assert_eq!(new_bare_name("1bad"), None);
        assert_eq!(new_bare_name("has space"), None);
        assert_eq!(new_bare_name(""), None);
    }
}
