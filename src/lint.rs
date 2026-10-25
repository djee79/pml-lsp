//! Semantic lints beyond syntax errors: locals that are assigned but never
//! read, and calls to methods that the receiver's definition does not provide.
//!
//! Both are deliberately conservative. Unused-variable checks run only inside
//! `define function` / `define method` bodies: file-level macro locals are
//! visible to the macros they call, so they are left alone. A scope with a
//! syntax error or a `$m` macro call is skipped too. Unknown-method checks
//! only fire when the receiver's type is a user object or form whose
//! definition is indexed; builtin types are not checked because the builtin
//! method list is not exhaustive. Argument counts are compared against every
//! definition of a user method, function or constructor, so overloads pass
//! as long as one of them fits.

use std::collections::HashSet;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, Position, Range};
use tree_sitter::{Node, Tree};

use crate::format::char_col;

/// Answers "does `receiver` have a method called `method`?" for the lint.
pub trait Resolver {
    /// `None` when the receiver's type is unknown or its method set is not
    /// fully known, so nothing should be reported. Otherwise the type's
    /// display name and whether the method exists on it.
    fn method_defined(&self, receiver: &str, method: &str) -> Option<(String, bool)>;
    /// Parameter counts of every user definition of `.method` on the
    /// receiver's type. `None` when the type is unknown or the method is not
    /// user-defined (builtin, universal, missing).
    fn method_arities(&self, receiver: &str, method: &str) -> Option<Vec<usize>>;
    /// Parameter counts of every definition of the user function `!!name`.
    fn function_arities(&self, name: &str) -> Option<Vec<usize>>;
    /// Parameter counts of the constructor methods of user object `type_name`.
    fn constructor_arities(&self, type_name: &str) -> Option<Vec<usize>>;
}

/// `!x` assigned inside a function or method body and never read there.
pub fn unused_variables(tree: &Tree, text: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "function_definition" | "method_definition") {
            if !node.has_error() {
                check_scope(node, text, &mut out);
            }
            continue;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    out
}

#[derive(Default)]
struct Scope {
    /// Assignments to locals: lowercase name, name as written, range.
    writes: Vec<(String, String, Range)>,
    /// Lowercase names of locals that are read anywhere in the scope.
    reads: HashSet<String>,
    /// A `$m` macro call can read the caller's locals: give up.
    bail: bool,
}

fn check_scope(scope_node: Node, text: &str, out: &mut Vec<Diagnostic>) {
    let mut scope = Scope::default();
    let lines: Vec<&str> = text.lines().collect();
    collect(scope_node, text, &lines, &mut scope);
    if scope.bail {
        return;
    }
    for (lc, written, range) in scope.writes {
        if !scope.reads.contains(&lc) {
            out.push(Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("pml-lsp".to_string()),
                message: format!("`{}` is assigned but never used", written),
                tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                ..Default::default()
            });
        }
    }
}

/// `!name` → `Some("!name")` lowercased, for locals only (`!!g` and `$!!g`
/// are globals; `!this` is never assigned).
fn local_key(var: &str) -> Option<String> {
    // Defensive: grammars before 1ea5b91 gave a `variable` right after `$P`
    // its leading space.
    let v = var.trim().trim_start_matches('$');
    if v.starts_with('!') && !v.starts_with("!!") && v.len() > 1 {
        Some(v.to_ascii_lowercase())
    } else {
        None
    }
}

fn collect(node: Node, text: &str, lines: &[&str], scope: &mut Scope) {
    let src = text.as_bytes();
    let mut skip: Option<usize> = None;
    match node.kind() {
        "assignment" | "var_statement" => {
            let field = if node.kind() == "assignment" { "left" } else { "variable" };
            if let Some(target) = node.child_by_field_name(field) {
                if target.kind() == "variable" {
                    let written = target.utf8_text(src).unwrap_or("");
                    if let Some(key) = local_key(written) {
                        scope.writes.push((key, written.to_string(), node_range(target, lines)));
                    }
                    skip = Some(target.id());
                }
            }
        }
        // Parameters and loop counters are bindings, not assignments: never
        // reported, and not counted as reads either.
        "parameter" => return,
        "do_loop" => skip = node.child_by_field_name("variable").map(|v| v.id()),
        "variable" => {
            if let Some(key) = local_key(node.utf8_text(src).unwrap_or("")) {
                scope.reads.insert(key);
            }
            return;
        }
        // `$!x` (and `!x` in a callback string) inside a string is a use.
        "string_literal" => {
            variables_in_string(node.utf8_text(src).unwrap_or(""), &mut scope.reads);
            return;
        }
        "dollar_directive" => {
            if node.utf8_text(src).unwrap_or("").to_ascii_lowercase().starts_with("$m") {
                scope.bail = true;
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) != skip {
            collect(child, text, lines, scope);
        }
    }
}

fn variables_in_string(s: &str, reads: &mut HashSet<String>) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let starts_name = |c: char| c.is_alphabetic() || c == '_';
        if chars[i] == '!' && (i == 0 || chars[i - 1] != '!') && chars.get(i + 1).map(|&c| starts_name(c)).unwrap_or(false) {
            let mut end = i + 1;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            let name: String = chars[i..end].iter().collect();
            reads.insert(name.to_ascii_lowercase());
            i = end;
        } else {
            i += 1;
        }
    }
}

/// `.method()` calls whose receiver is a known user type without that method.
pub fn unknown_methods(tree: &Tree, text: &str, resolver: &dyn Resolver) -> Vec<Diagnostic> {
    let src = text.as_bytes();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "method_call" && !node.has_error() {
            if let (Some(object), Some(method)) = (node.child_by_field_name("object"), node.child_by_field_name("method")) {
                let receiver = object.utf8_text(src).unwrap_or("");
                let name = method.utf8_text(src).unwrap_or("");
                if let Some((owner, false)) = resolver.method_defined(receiver, name) {
                    out.push(Diagnostic {
                        range: node_range(method, &lines),
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("pml-lsp".to_string()),
                        message: format!("Method `.{}()` is not defined for {}", name, owner),
                        ..Default::default()
                    });
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    out
}

/// Calls whose argument count matches none of the callee's definitions.
pub fn argument_counts(tree: &Tree, text: &str, resolver: &dyn Resolver) -> Vec<Diagnostic> {
    let src = text.as_bytes();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if !node.has_error() {
            let text_of = |n: Node| n.utf8_text(src).unwrap_or("").to_string();
            let checked: Option<(String, Vec<usize>, Node)> = match node.kind() {
                "method_call" => match (node.child_by_field_name("object"), node.child_by_field_name("method"), args_of(node)) {
                    (Some(obj), Some(m), Some(args)) => {
                        resolver.method_arities(&text_of(obj), &text_of(m)).map(|a| (format!(".{}()", text_of(m)), a, args))
                    }
                    _ => None,
                },
                "function_call" => match (node.child_by_field_name("function"), args_of(node)) {
                    (Some(f), Some(args)) if f.kind() == "variable" && text_of(f).starts_with("!!") => {
                        resolver.function_arities(&text_of(f)).map(|a| (format!("{}()", text_of(f)), a, args))
                    }
                    _ => None,
                },
                "object_expression" => match (node.child_by_field_name("type"), args_of(node)) {
                    // `object T()` always works even when T has only
                    // constructors with arguments, so an empty list is fine.
                    (Some(t), Some(args)) if argument_count(args) > 0 => {
                        resolver.constructor_arities(&text_of(t)).map(|a| (format!("object {}()", text_of(t)), a, args))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some((callee, arities, args)) = checked {
                let given = argument_count(args);
                if !arities.is_empty() && !arities.contains(&given) {
                    out.push(Diagnostic {
                        range: node_range(args, &lines),
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("pml-lsp".to_string()),
                        message: format!("`{}` takes {}, but {} given", callee, describe_arities(&arities), describe_given(given)),
                        ..Default::default()
                    });
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    out
}

fn args_of<'a>(call: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = call.walk();
    let found = call.children(&mut cursor).find(|c| c.kind() == "argument_list");
    found
}

fn argument_count(argument_list: Node) -> usize {
    let mut cursor = argument_list.walk();
    argument_list.named_children(&mut cursor).filter(|c| c.kind() == "expression").count()
}

fn describe_arities(arities: &[usize]) -> String {
    let mut sorted: Vec<usize> = arities.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
    match sorted.as_slice() {
        [0] => "no arguments".to_string(),
        [n] => format!("{} {}", n, plural(*n)),
        [rest @ .., last] => format!(
            "{} or {} {}",
            rest.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", "),
            last,
            plural(*last)
        ),
        [] => "no arguments".to_string(),
    }
}

fn describe_given(given: usize) -> String {
    match given {
        0 => "none".to_string(),
        1 => "1 was".to_string(),
        n => format!("{} were", n),
    }
}

fn node_range(node: Node, lines: &[&str]) -> Range {
    let (s, e) = (node.start_position(), node.end_position());
    let col = |row: usize, byte: usize| lines.get(row).map(|l| char_col(l, byte)).unwrap_or(byte) as u32;
    Range::new(Position::new(s.row as u32, col(s.row, s.column)), Position::new(e.row as u32, col(e.row, e.column)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::PmlParser;

    fn unused(src: &str) -> Vec<(u32, String)> {
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        unused_variables(&tree, src).into_iter().map(|d| (d.range.start.line, d.message)).collect()
    }

    #[test]
    fn reports_locals_never_read() {
        let src = "define method .m(!p is REAL)\n  !a = 1\n  !b = 2\n  !c = !b + 1\n  !d = 0\n  !d = !d + 1\n  var !e coll all\n  !!g = 5\nendmethod\n";
        assert_eq!(
            unused(src),
            vec![
                (1, "`!a` is assigned but never used".to_string()),
                (3, "`!c` is assigned but never used".to_string()),
                (6, "`!e` is assigned but never used".to_string()),
            ]
        );
    }

    #[test]
    fn reads_in_strings_output_commands_and_calls_count() {
        let src = "define method .m()\n  !a = 1\n  !b = 2\n  !c = 3\n  !d = ARRAY()\n  !e = 7\n  !f = 1\n  !g = 1\n  $P value $!a\n  $P $!g\n  !!f.text.val = |n=$!b|\n  q var !c\n  !d.append(1)\n  !e[1] = 2\n  !!f.cb.callback = '!f.go()'\nendmethod\n";
        assert!(unused(src).is_empty());
    }

    #[test]
    fn case_insensitive_and_self_reference() {
        assert!(unused("define method .m()\n  !Count = 0\n  !count = !COUNT + 1\nendmethod\n").is_empty());
    }

    #[test]
    fn loop_counters_parameters_and_file_level_locals_are_ignored() {
        let src = "!top = 1\ndefine function !!f(!unusedParam is STRING)\n  do !i from 1 to 3\n    !x = 1\n  enddo\nendfunction\n";
        assert_eq!(unused(src), vec![(3, "`!x` is assigned but never used".to_string())]);
    }

    #[test]
    fn macro_calls_and_syntax_errors_disable_the_check() {
        assert!(unused("define method .m()\n  !a = 1\n  $m/path/to/macro\nendmethod\n").is_empty());
        assert!(unused("define method .m()\n  !a = 1\n  !b = = 2\nendmethod\n").is_empty());
    }

    struct Fake;
    impl Resolver for Fake {
        fn method_defined(&self, receiver: &str, method: &str) -> Option<(String, bool)> {
            match receiver.to_ascii_lowercase().as_str() {
                "!this" => Some(("!!myForm".into(), method.eq_ignore_ascii_case("ok"))),
                "!box" => Some(("BOX".into(), method.eq_ignore_ascii_case("volume"))),
                _ => None,
            }
        }
        fn method_arities(&self, receiver: &str, method: &str) -> Option<Vec<usize>> {
            match (receiver.to_ascii_lowercase().as_str(), method.to_ascii_lowercase().as_str()) {
                ("!this", "ok") => Some(vec![0]),
                ("!box", "volume") => Some(vec![1, 2]),
                ("!box", "show") => None, // builtin: arity unknown
                _ => None,
            }
        }
        fn function_arities(&self, name: &str) -> Option<Vec<usize>> {
            name.eq_ignore_ascii_case("!!area").then(|| vec![2])
        }
        fn constructor_arities(&self, type_name: &str) -> Option<Vec<usize>> {
            type_name.eq_ignore_ascii_case("box").then(|| vec![1])
        }
    }

    #[test]
    fn reports_argument_count_mismatches() {
        let src = "define method .m()\n  !this.ok()\n  !this.ok(1)\n  !box.volume(1)\n  !box.volume(1, 2)\n  !box.volume()\n  !box.show(1, 2, 3)\n  !a = !!Area(1, 2)\n  !b = !!Area(!x.y(1), $\n    2, 3)\n  !c = !!Unknown(1)\n  !d = object BOX()\n  !e = object BOX(1)\n  !f = object BOX(1, 2)\n  !g = object STRING(1)\nendmethod\n";
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        let d = argument_counts(&tree, src, &Fake);
        let got: Vec<(u32, String)> = d.iter().map(|x| (x.range.start.line, x.message.clone())).collect();
        assert_eq!(
            got,
            vec![
                (2, "`.ok()` takes no arguments, but 1 was given".to_string()),
                (5, "`.volume()` takes 1 or 2 arguments, but none given".to_string()),
                (8, "`!!Area()` takes 2 arguments, but 3 were given".to_string()),
                (13, "`object BOX()` takes 1 argument, but 2 were given".to_string()),
            ]
        );
        // The range covers the argument list, including the continuation.
        assert_eq!((d[2].range.start.character, d[2].range.end.line), (13, 9));
    }

    #[test]
    fn reports_unknown_methods_on_known_receivers_only() {
        let src = "define method .m()\n  !this.ok()\n  !this.OK()\n  !this.resize(1)\n  !v = !box.volume() + !box.area()\n  !s.upcase()\n  !this.ok().foo()\nendmethod\n";
        let parser = PmlParser::new();
        let tree = parser.parse(src).unwrap();
        let d = unknown_methods(&tree, src, &Fake);
        let got: Vec<(u32, u32, String)> = d.iter().map(|x| (x.range.start.line, x.range.start.character, x.message.clone())).collect();
        assert_eq!(
            got,
            vec![
                (3, 8, "Method `.resize()` is not defined for !!myForm".to_string()),
                (4, 28, "Method `.area()` is not defined for BOX".to_string()),
            ]
        );
        assert_eq!(d[0].range.end.character, 14);
    }
}
