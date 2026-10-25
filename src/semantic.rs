//! Semantic tokens: highlighting that needs symbol knowledge the grammar does
//! not have. Which `!!name` is a function, a form or a global; which `.name`
//! is a gadget, a member, a user method or a builtin method; which type names
//! are user objects.

use std::collections::HashSet;
use tower_lsp::lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType};

pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::CLASS,    // 0: user object, form
    SemanticTokenType::FUNCTION, // 1: !!function
    SemanticTokenType::METHOD,   // 2: .method
    SemanticTokenType::PROPERTY, // 3: .member, .gadget
    SemanticTokenType::VARIABLE, // 4: !!global
    SemanticTokenType::TYPE,     // 5: builtin type name
];

pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION,     // bit 0
    SemanticTokenModifier::DEFAULT_LIBRARY, // bit 1
    SemanticTokenModifier::STATIC,          // bit 2
];

const CLASS: u32 = 0;
const FUNCTION: u32 = 1;
const METHOD: u32 = 2;
const PROPERTY: u32 = 3;
const VARIABLE: u32 = 4;
const TYPE: u32 = 5;
const DECLARATION: u32 = 1;
const DEFAULT_LIBRARY: u32 = 2;
const STATIC: u32 = 4;

/// Lowercase name sets the classifier consults.
#[derive(Default, Debug)]
pub struct Names {
    pub functions: HashSet<String>,
    pub forms: HashSet<String>,
    pub globals: HashSet<String>,
    pub methods: HashSet<String>,
    pub members: HashSet<String>,
    pub gadgets: HashSet<String>,
    pub objects: HashSet<String>,
    pub builtin_methods: HashSet<String>,
    pub builtin_types: HashSet<String>,
}

impl Names {
    fn has(set: &HashSet<String>, name: &str) -> bool {
        set.contains(&name.to_ascii_lowercase())
    }
}

/// Absolute token before delta encoding.
#[derive(Debug, PartialEq, Eq)]
pub struct Token {
    pub line: u32,
    /// UTF-16 column.
    pub start: u32,
    pub length: u32,
    pub token_type: u32,
    pub modifiers: u32,
}

pub fn tokens(text: &str, names: &Names) -> Vec<SemanticToken> {
    encode(scan(text, names))
}

fn scan(text: &str, names: &Names) -> Vec<Token> {
    let mut out = Vec::new();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    for (row, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        // UTF-16 column of each char index, plus one past the end.
        let mut cols = Vec::with_capacity(chars.len() + 1);
        let mut col = 0u32;
        for c in &chars {
            cols.push(col);
            col += c.len_utf16() as u32;
        }
        cols.push(col);

        let lower = line.to_ascii_lowercase();
        let lower_chars: Vec<char> = lower.chars().collect();
        let mut quote: Option<char> = None;
        let mut i = 0;
        let mut prev_word: Option<String> = None;

        while i < chars.len() {
            let c = chars[i];
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            match c {
                '\'' | '|' => {
                    quote = Some(c);
                    i += 1;
                }
                '-' if chars.get(i + 1) == Some(&'-') => break,
                '$' if chars.get(i + 1) == Some(&'*') => break,
                '!' | '$' if i + 1 < chars.len() && matches!(chars[i + 1], '!' | '$' | 'a'..='z' | 'A'..='Z' | '_') => {
                    let start = i;
                    while i < chars.len() && matches!(chars[i], '!' | '$') {
                        i += 1;
                    }
                    let bangs = chars[start..i].iter().filter(|&&x| x == '!').count();
                    let name_start = i;
                    while i < chars.len() && is_word(chars[i]) {
                        i += 1;
                    }
                    let name: String = lower_chars[name_start..i].iter().collect();
                    if bangs == 2 && !name.is_empty() {
                        let full = format!("!!{}", name);
                        let (tt, m) = if Names::has(&names.functions, &full) {
                            (FUNCTION, 0)
                        } else if Names::has(&names.forms, &full) {
                            (CLASS, 0)
                        } else if Names::has(&names.globals, &full) {
                            (VARIABLE, STATIC)
                        } else {
                            prev_word = None;
                            continue;
                        };
                        out.push(Token { line: row as u32, start: cols[name_start], length: cols[i] - cols[name_start], token_type: tt, modifiers: m });
                    }
                    prev_word = None;
                }
                '.' if i + 1 < chars.len() && (chars[i + 1].is_alphabetic() || chars[i + 1] == '_') => {
                    let name_start = i + 1;
                    let mut j = name_start;
                    while j < chars.len() && is_word(chars[j]) {
                        j += 1;
                    }
                    let name: String = lower_chars[name_start..j].iter().collect();
                    let dotted = format!(".{}", name);
                    let is_call = chars.get(j) == Some(&'(');
                    let declaration = matches!(prev_word.as_deref(), Some("method") | Some("member"))
                        || names.gadgets.contains(&dotted) && prev_word.is_some() && !is_call;
                    let decl_mod = if declaration { DECLARATION } else { 0 };
                    let tok = if is_call && Names::has(&names.methods, &dotted) {
                        Some((METHOD, decl_mod))
                    } else if is_call && Names::has(&names.builtin_methods, &name) {
                        Some((METHOD, DEFAULT_LIBRARY))
                    } else if !is_call && Names::has(&names.methods, &dotted) && prev_word.as_deref() == Some("method") {
                        Some((METHOD, DECLARATION))
                    } else if Names::has(&names.gadgets, &dotted) || Names::has(&names.members, &dotted) {
                        Some((PROPERTY, decl_mod))
                    } else {
                        None
                    };
                    if let Some((tt, m)) = tok {
                        out.push(Token { line: row as u32, start: cols[name_start], length: cols[j] - cols[name_start], token_type: tt, modifiers: m });
                    }
                    i = j;
                    prev_word = None;
                }
                c if c.is_alphabetic() || c == '_' => {
                    let start = i;
                    while i < chars.len() && is_word(chars[i]) {
                        i += 1;
                    }
                    let word: String = lower_chars[start..i].iter().collect();
                    let after_type_keyword = matches!(prev_word.as_deref(), Some("object") | Some("is"));
                    if after_type_keyword {
                        if Names::has(&names.objects, &word) {
                            let decl = if prev_word.as_deref() == Some("object") && line.to_ascii_lowercase().trim_start().starts_with("define ") { DECLARATION } else { 0 };
                            out.push(Token { line: row as u32, start: cols[start], length: cols[i] - cols[start], token_type: CLASS, modifiers: decl });
                        } else if Names::has(&names.builtin_types, &word) || prev_word.as_deref() == Some("is") {
                            out.push(Token { line: row as u32, start: cols[start], length: cols[i] - cols[start], token_type: TYPE, modifiers: DEFAULT_LIBRARY });
                        }
                    } else if Names::has(&names.builtin_types, &word) && chars.get(i) == Some(&'(') {
                        out.push(Token { line: row as u32, start: cols[start], length: cols[i] - cols[start], token_type: TYPE, modifiers: DEFAULT_LIBRARY });
                    }
                    prev_word = Some(word);
                }
                ' ' | '\t' => i += 1,
                _ => {
                    i += 1;
                    prev_word = None;
                }
            }
        }
    }
    out
}

fn encode(mut toks: Vec<Token>) -> Vec<SemanticToken> {
    toks.sort_by_key(|t| (t.line, t.start));
    let mut out = Vec::with_capacity(toks.len());
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for t in toks {
        let delta_line = t.line - prev_line;
        let delta_start = if delta_line == 0 { t.start - prev_start } else { t.start };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.length,
            token_type: t.token_type,
            token_modifiers_bitset: t.modifiers,
        });
        prev_line = t.line;
        prev_start = t.start;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        Names {
            functions: set(&["!!area"]),
            forms: set(&["!!myform"]),
            globals: set(&["!!count"]),
            methods: set(&[".scale", ".okpressed"]),
            members: set(&[".width"]),
            gadgets: set(&[".ok", ".name"]),
            objects: set(&["box"]),
            builtin_methods: set(&["upcase", "val"]),
            builtin_types: set(&["string", "real", "array"]),
        }
    }

    fn kinds(text: &str) -> Vec<(u32, u32, u32, u32)> {
        scan(text, &names()).into_iter().map(|t| (t.line, t.start, t.token_type, t.modifiers)).collect()
    }

    #[test]
    fn classifies_globals_methods_and_properties() {
        let t = "!x = !!Area(1) + !!count\n!!myForm.ok.val\n!b.scale(2)\n!s = !s.upcase()\n";
        assert_eq!(
            kinds(t),
            vec![
                (0, 7, FUNCTION, 0),
                (0, 19, VARIABLE, STATIC),
                (1, 2, CLASS, 0),
                (1, 9, PROPERTY, 0),
                (2, 3, METHOD, 0),
                (3, 8, METHOD, DEFAULT_LIBRARY),
            ]
        );
    }

    #[test]
    fn declarations_and_types() {
        let t = "define object BOX\n  member .width is REAL\nendobject\ndefine method .scale(!f is REAL)\n!b = object BOX()\nbutton .ok |OK|\n";
        let k = kinds(t);
        assert!(k.contains(&(0, 14, CLASS, DECLARATION)));
        assert!(k.contains(&(1, 10, PROPERTY, DECLARATION)));
        assert!(k.contains(&(1, 19, TYPE, DEFAULT_LIBRARY)));
        assert!(k.contains(&(3, 15, METHOD, DECLARATION)));
        assert!(k.contains(&(3, 27, TYPE, DEFAULT_LIBRARY)));
        assert!(k.contains(&(4, 12, CLASS, 0)));
        assert!(k.contains(&(5, 8, PROPERTY, DECLARATION)));
    }

    #[test]
    fn skips_comments_and_strings() {
        let t = "!x = '!!Area' -- !!Area !b.scale()\n$* !!count\n";
        assert!(kinds(t).is_empty());
    }

    #[test]
    fn delta_encodes_in_order() {
        let enc = tokens("!!count\n!x = !!Area(1)\n", &names());
        assert_eq!(enc.len(), 2);
        assert_eq!((enc[0].delta_line, enc[0].delta_start, enc[0].length), (0, 2, 5));
        assert_eq!((enc[1].delta_line, enc[1].delta_start, enc[1].length), (1, 7, 4));
    }
}
