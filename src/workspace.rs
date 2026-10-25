//! Workspace index: symbols of every PML file under the project root(s), so
//! completion, hover and go-to-definition work across files.

use crate::symbols::SymbolTable;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::Url;
use tree_sitter::Parser;

pub const PML_EXTENSIONS: &[&str] = &["pml", "pmlfnc", "pmlobj", "pmlfrm", "pmlmac", "pmlcmd", "mac"];

/// Glob for `workspace/didChangeWatchedFiles` registration.
pub const WATCH_GLOB: &str = "**/*.{pml,pmlfnc,pmlobj,pmlfrm,pmlmac,pmlcmd,mac,PML,PMLFNC,PMLOBJ,PMLFRM,PMLMAC,PMLCMD,MAC}";

const SKIP_DIRS: &[&str] = &[".git", ".svn", "node_modules", "target", ".venv", "__pycache__"];
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 50_000;

pub fn is_pml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| PML_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// All PML files under `root`, recursively, skipping VCS and build folders.
pub fn find_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                    stack.push(path);
                }
            } else if meta.is_file() && meta.len() <= MAX_FILE_BYTES && is_pml_file(&path) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

/// Parse one file and extract its symbols. `None` if unreadable.
pub fn index_file(parser: &mut Parser, path: &Path) -> Option<(Url, SymbolTable)> {
    let bytes = std::fs::read(path).ok()?;
    // PML files are usually Windows-1252; replace undecodable bytes rather than fail.
    let text = String::from_utf8_lossy(&bytes);
    let tree = parser.parse(text.as_ref(), None)?;
    let url = Url::from_file_path(path.canonicalize().unwrap_or_else(|_| path.to_path_buf())).ok()?;
    Some((url, SymbolTable::extract(&tree, &text)))
}

/// Index every PML file under `root`. Blocking; run on a worker thread.
pub fn index_dir(root: &Path) -> Vec<(Url, SymbolTable)> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_pml::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    find_files(root)
        .iter()
        .filter_map(|p| index_file(&mut parser, p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pml-lsp-test-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("lib/area.pmlfnc"), "define function !!Area(!l is REAL) is REAL\n  return !l\nendfunction\n").unwrap();
        std::fs::write(dir.join("box.pmlobj"), "define object BOX\n  member .w is REAL\nendobject\ndefine method .area() is REAL\nendmethod\n").unwrap();
        std::fs::write(dir.join(".git/ignored.pmlfnc"), "define function !!Hidden()\nendfunction\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "define function !!NotPml()\nendfunction\n").unwrap();
        dir
    }

    #[test]
    fn finds_only_pml_files_outside_skipped_dirs() {
        let dir = temp_project("find");
        let files = find_files(&dir);
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, vec!["box.pmlobj", "area.pmlfnc"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexes_symbols_with_owners() {
        let dir = temp_project("index");
        let indexed = index_dir(&dir);
        assert_eq!(indexed.len(), 2);
        let all: Vec<&crate::symbols::Symbol> = indexed.iter().flat_map(|(_, t)| t.iter()).collect();
        assert!(all.iter().any(|s| s.name == "!!Area"));
        let area = all.iter().find(|s| s.name == ".area").unwrap();
        assert_eq!(area.owner.as_deref(), Some("BOX"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recognises_extensions_case_insensitively() {
        assert!(is_pml_file(Path::new("X.PMLFRM")));
        assert!(is_pml_file(Path::new("x.mac")));
        assert!(!is_pml_file(Path::new("x.txt")));
    }
}
