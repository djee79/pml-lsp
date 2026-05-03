//! Tree-sitter parsing layer.
//!
//! Wraps a single `tree_sitter::Parser` per Backend and provides a convenient
//! `parse` that returns a `Tree`.

use std::sync::Mutex;
use tree_sitter::{Parser, Tree};

pub struct PmlParser {
    inner: Mutex<Parser>,
}

impl PmlParser {
    /// Construct a new parser configured with the PML grammar.
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_aveva_pml_parser::LANGUAGE.into())
            .expect("failed to load PML grammar — check tree-sitter-aveva-pml-parser binding build");
        Self {
            inner: Mutex::new(parser),
        }
    }

    /// Parse a complete document and return the resulting tree.
    pub fn parse(&self, text: &str) -> Option<Tree> {
        let mut parser = self.inner.lock().expect("parser mutex poisoned");
        parser.parse(text, None)
    }
}

impl Default for PmlParser {
    fn default() -> Self {
        Self::new()
    }
}
