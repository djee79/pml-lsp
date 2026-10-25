//! Static type inference for PML expressions, as far as text allows.
//!
//! `infer("!this.name.val.upcase()")` walks the chain: `!this` is the form,
//! `.name` a TEXT gadget, `.val` its STRING value, `.upcase()` a STRING
//! method returning STRING. Type names are either builtin (`STRING`, `FORM`,
//! `BUTTON`) or user object / form names (`BOX`, `!!myForm`).

use crate::context::Decl;

const MAX_DEPTH: usize = 6;

/// What the inference needs to know about the program.
pub trait Env {
    /// Declaration of a variable in the current file.
    fn declared(&self, var: &str) -> Option<Decl>;
    /// Type of `!this`: the object or form defined in the current file.
    fn this_type(&self) -> Option<String>;
    /// Type of a global: a form name is its own type.
    fn global_type(&self, name: &str) -> Option<String>;
    /// Type of `.member` or `.gadget` on `owner`.
    fn member_type(&self, owner: &str, member: &str) -> Option<String>;
    /// Return type of `.method()` on `owner` (user or builtin).
    fn method_return(&self, owner: &str, method: &str) -> Option<String>;
    /// Return type of a `!!function()`.
    fn function_return(&self, name: &str) -> Option<String>;
}

pub fn infer(expr: &str, env: &dyn Env) -> Option<String> {
    infer_depth(expr, env, 0)
}

fn infer_depth(expr: &str, env: &dyn Env, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let expr = expr.trim();
    let chars: Vec<char> = expr.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    // Literals and whole-expression operators first.
    let lower = expr.to_ascii_lowercase();
    if let Some(t) = operator_type(&lower) {
        return Some(t);
    }
    match chars[0] {
        '\'' | '|' => return Some("STRING".into()),
        c if c.is_ascii_digit() || c == '-' || (c == '.' && chars.get(1).map(|d| d.is_ascii_digit()).unwrap_or(false)) => {
            return Some("REAL".into())
        }
        '(' => {
            if let Some(close) = matching_paren(&chars, 0) {
                if close == chars.len() - 1 {
                    return infer_depth(&expr[1..expr.len() - 1], env, depth + 1);
                }
            }
        }
        _ => {}
    }
    if lower == "true" || lower == "false" {
        return Some("BOOLEAN".into());
    }

    // Head of the chain.
    let mut i = 0;
    let mut current: Option<String>;
    if lower.starts_with("object ") {
        i = 7;
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        let t: String = chars[i..].iter().take_while(|&&c| is_word(c)).collect();
        i += t.chars().count();
        current = (!t.is_empty()).then(|| t.to_ascii_uppercase());
        i = skip_parens(&chars, i);
    } else if chars[0] == '!' || chars[0] == '$' {
        while i < chars.len() && matches!(chars[i], '!' | '$') {
            i += 1;
        }
        let name: String = chars[i..].iter().take_while(|&&c| is_word(c)).collect();
        i += name.chars().count();
        let prefix: String = chars[..i - name.chars().count()].iter().filter(|&&c| c == '!').collect();
        let var = format!("{}{}", prefix, name);
        if var.eq_ignore_ascii_case("!this") {
            current = env.this_type();
        } else if prefix == "!!" && chars.get(i) == Some(&'(') {
            current = env.function_return(&var);
            i = skip_parens(&chars, i);
        } else {
            current = env.global_type(&var).or_else(|| match env.declared(&var)? {
                Decl::Type(t) => Some(t),
                Decl::Expr(e) => infer_depth(&e, env, depth + 1),
            });
        }
    } else if is_word(chars[0]) {
        // `STRING('x')`, `ARRAY()`: builtin constructor.
        let name: String = chars.iter().take_while(|&&c| is_word(c)).collect();
        i = name.chars().count();
        if chars.get(i) != Some(&'(') {
            return None;
        }
        current = Some(name.to_ascii_uppercase());
        i = skip_parens(&chars, i);
    } else {
        return None;
    }

    // Postfix chain: .member, .method(...), [index]
    while i < chars.len() {
        match chars[i] {
            ' ' => i += 1,
            '.' => {
                let name: String = chars[i + 1..].iter().take_while(|&&c| is_word(c)).collect();
                if name.is_empty() {
                    return None;
                }
                i += 1 + name.chars().count();
                let owner = current?;
                if chars.get(i) == Some(&'(') {
                    i = skip_parens(&chars, i);
                    current = env.method_return(&owner, &name);
                } else {
                    current = env.member_type(&owner, &name);
                }
            }
            '[' => {
                // Element of an array: unknown.
                return None;
            }
            _ => return None,
        }
    }
    current
}

/// Type implied by a top-level operator in the expression, if any.
fn operator_type(lower: &str) -> Option<String> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let chars: Vec<char> = lower.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    for (i, &c) in chars.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                continue;
            }
            None => {}
        }
        match c {
            '\'' | '|' => quote = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth > 0 => {}
            '&' => return Some("STRING".into()),
            '+' | '*' | '/' if i > 0 => return Some("REAL".into()),
            '-' if i > 0 && chars[i - 1] == ' ' => return Some("REAL".into()),
            '=' | '<' | '>' if i > 0 => return Some("BOOLEAN".into()),
            ' ' => {
                let word: String = chars[i + 1..].iter().take_while(|&&c| is_word(c)).collect();
                let after = chars.get(i + 1 + word.len()).copied().unwrap_or(' ');
                if after == ' ' && matches!(word.as_str(), "eq" | "ne" | "gt" | "lt" | "ge" | "le" | "and" | "or") {
                    return Some("BOOLEAN".into());
                }
            }
            _ => {}
        }
    }
    None
}

fn matching_paren(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, &c) in chars.iter().enumerate().skip(open) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '|' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index just past the `(...)` starting at `i`, or `i` if there is none.
fn skip_parens(chars: &[char], i: usize) -> usize {
    if chars.get(i) == Some(&'(') {
        matching_paren(chars, i).map(|c| c + 1).unwrap_or(chars.len())
    } else {
        i
    }
}

/// `define method .area() is REAL` / `UpCase() IS STRING` -> `REAL` / `STRING`.
pub fn return_type_of(detail: &str) -> Option<String> {
    let close = detail.rfind(')')?;
    let tail = detail[close + 1..].trim();
    if tail.len() < 3 || !tail[..2].eq_ignore_ascii_case("is") {
        return None;
    }
    let t: String = tail[2..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!t.is_empty()).then(|| t.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock;
    impl Env for Mock {
        fn declared(&self, var: &str) -> Option<Decl> {
            match var.to_ascii_lowercase().as_str() {
                "!a" => Some(Decl::Type("STRING".into())),
                "!b" => Some(Decl::Expr("object BOX()".into())),
                "!c" => Some(Decl::Expr("!a.upcase()".into())),
                "!d" => Some(Decl::Expr("!this.name.val".into())),
                "!e" => Some(Decl::Expr("!b.area() * 2".into())),
                "!f" => Some(Decl::Expr("!!Area(1, 2)".into())),
                "!g" => Some(Decl::Expr("!g.upcase()".into())),
                "!h" => Some(Decl::Expr("('x' & !a)".into())),
                "!arr" => Some(Decl::Expr("ARRAY()".into())),
                "!i" => Some(Decl::Expr("!arr[1]".into())),
                _ => None,
            }
        }
        fn this_type(&self) -> Option<String> {
            Some("!!myForm".into())
        }
        fn global_type(&self, name: &str) -> Option<String> {
            name.eq_ignore_ascii_case("!!myForm").then(|| "!!myForm".to_string())
        }
        fn member_type(&self, owner: &str, member: &str) -> Option<String> {
            match (owner, member.to_ascii_lowercase().as_str()) {
                ("!!myForm", "name") => Some("TEXT".into()),
                ("TEXT", "val") => Some("STRING".into()),
                ("BOX", "width") => Some("REAL".into()),
                _ => None,
            }
        }
        fn method_return(&self, owner: &str, method: &str) -> Option<String> {
            match (owner, method.to_ascii_lowercase().as_str()) {
                ("STRING", "upcase") => Some("STRING".into()),
                ("BOX", "area") => Some("REAL".into()),
                _ => None,
            }
        }
        fn function_return(&self, name: &str) -> Option<String> {
            name.eq_ignore_ascii_case("!!Area").then(|| "REAL".to_string())
        }
    }

    fn t(e: &str) -> Option<String> {
        infer(e, &Mock)
    }

    #[test]
    fn literals_and_constructors() {
        assert_eq!(t("'x'").as_deref(), Some("STRING"));
        assert_eq!(t("|x|").as_deref(), Some("STRING"));
        assert_eq!(t("12.5").as_deref(), Some("REAL"));
        assert_eq!(t("-1").as_deref(), Some("REAL"));
        assert_eq!(t("TRUE").as_deref(), Some("BOOLEAN"));
        assert_eq!(t("object BOX()").as_deref(), Some("BOX"));
        assert_eq!(t("STRING('a')").as_deref(), Some("STRING"));
        assert_eq!(t("ARRAY()").as_deref(), Some("ARRAY"));
    }

    #[test]
    fn variables_and_chains() {
        assert_eq!(t("!a").as_deref(), Some("STRING"));
        assert_eq!(t("!b").as_deref(), Some("BOX"));
        assert_eq!(t("!b.width").as_deref(), Some("REAL"));
        assert_eq!(t("!c").as_deref(), Some("STRING"), "through method return type");
        assert_eq!(t("!d").as_deref(), Some("STRING"), "!this -> gadget -> val");
        assert_eq!(t("!this.name").as_deref(), Some("TEXT"));
        assert_eq!(t("!this.name.val.upcase()").as_deref(), Some("STRING"));
        assert_eq!(t("!!myForm.name").as_deref(), Some("TEXT"));
        assert_eq!(t("!f").as_deref(), Some("REAL"), "function return type");
        assert_eq!(t("!a.upcase().upcase()").as_deref(), Some("STRING"));
    }

    #[test]
    fn operators_and_parens() {
        assert_eq!(t("!e").as_deref(), Some("REAL"));
        assert_eq!(t("!h").as_deref(), Some("STRING"));
        assert_eq!(t("'a' & !b.width").as_deref(), Some("STRING"));
        assert_eq!(t("!b.width gt 1").as_deref(), Some("BOOLEAN"));
        assert_eq!(t("(!a)").as_deref(), Some("STRING"));
        assert_eq!(t("!a.upcase('(', 1) ").as_deref(), Some("STRING"), "parens in args");
    }

    #[test]
    fn gives_up_cleanly() {
        assert_eq!(t("!g"), None, "self-referential declaration");
        assert_eq!(t("!i"), None, "array element");
        assert_eq!(t("!unknown.x"), None);
        assert_eq!(t("!b.nothing()"), None);
        assert_eq!(t(""), None);
    }

    #[test]
    fn parses_return_types() {
        assert_eq!(return_type_of("define method .area() is REAL").as_deref(), Some("REAL"));
        assert_eq!(return_type_of("UpCase() IS STRING").as_deref(), Some("STRING"));
        assert_eq!(return_type_of("Replace(STRING a, STRING b) IS STRING").as_deref(), Some("STRING"));
        assert_eq!(return_type_of("define method .scale(!f is REAL)"), None);
    }
}
