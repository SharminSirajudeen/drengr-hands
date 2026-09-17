//! What every source-reading guard needs, in one place.
//!
//! Guards in this codebase assert a structural property by reading the tree as
//! text: no second adb spawn, no ptrace request that traces us, no fabricated
//! NetworkEvent field, no process that inherits the MCP transport. Each needs
//! the same two things, to walk the sources and to blank the parts of a file
//! that must not match a needle.
//!
//! They were copied instead. Five `code_only` bodies and seven directory walkers
//! existed at once, and one of them already lived here in spirit as
//! `contract_test::code_only`, which the later copies were written beside rather
//! than reused. A guard that polices duplication has no business being the most
//! duplicated code in the tree.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, recursively, as (path relative to `dir`, contents).
/// Unreadable entries are skipped: a guard reports on what it can read, and its
/// own self-check is what catches reading too little.
pub fn rust_files_under(dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    files
        .into_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(dir)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, std::fs::read_to_string(&p).ok()?))
        })
        .collect()
}

/// `src/`, the root every guard scans.
pub fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The source with comments blanked to spaces.
///
/// Use when the needle is a code construct and prose may legitimately name it,
/// which is most guards: they document the defect they prevent, and that
/// documentation must never trip them.
pub fn without_comments(src: &str) -> String {
    blank(src, false)
}

/// The source with comments AND string/char literal interiors blanked.
///
/// Use when the needle could appear inside a literal, so brace matching and
/// searches only ever see code.
pub fn without_comments_or_strings(src: &str) -> String {
    blank(src, true)
}

/// Length is preserved in both modes, so byte offsets and line numbers computed
/// on the result still address the original.
fn blank(src: &str, strings_too: bool) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = b.clone();
    let mut i = 0;

    let wipe = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in out.iter_mut().take(to).skip(from) {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };

    while i < b.len() {
        match (b[i], b.get(i + 1).copied().unwrap_or('\0')) {
            ('/', '/') => {
                let end = b[i..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(b.len(), |p| i + p);
                wipe(&mut out, i, end);
                i = end;
            }
            ('/', '*') => {
                let mut j = i + 2;
                while j + 1 < b.len() && !(b[j] == '*' && b[j + 1] == '/') {
                    j += 1;
                }
                let end = (j + 2).min(b.len());
                wipe(&mut out, i, end);
                i = end;
            }
            ('"', _) if strings_too => {
                let mut j = i + 1;
                while j < b.len() && b[j] != '"' {
                    j += if b[j] == '\\' { 2 } else { 1 };
                }
                wipe(&mut out, i + 1, j.min(b.len()));
                i = (j + 1).min(b.len());
            }
            _ => i += 1,
        }
    }
    out.into_iter().collect()
}

/// The 1-based line number of a byte offset into text produced by `blank`.
pub fn line_of(code: &str, at: usize) -> usize {
    code[..at].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_blanked_and_code_is_not() {
        let out = without_comments("let a = 1; // PTRACE_TRACEME\nlet b = 2;");
        assert!(
            !out.contains("PTRACE_TRACEME"),
            "prose must not match a needle"
        );
        assert!(out.contains("let a = 1;") && out.contains("let b = 2;"));
    }

    #[test]
    fn string_interiors_survive_unless_asked() {
        let src = r#"let m = "?";"#;
        assert!(
            without_comments(src).contains('?'),
            "a literal is code to most guards"
        );
        assert!(!without_comments_or_strings(src).contains('?'));
    }

    #[test]
    fn length_is_preserved_so_offsets_still_address_the_original() {
        let src = "a // comment\nb /* block */ c\nlet s = \"xy\";";
        for f in [without_comments, without_comments_or_strings] {
            assert_eq!(f(src).chars().count(), src.chars().count());
        }
    }

    #[test]
    fn line_numbers_survive_blanking() {
        let src = "one\n// two\nthree";
        let code = without_comments(src);
        assert_eq!(line_of(&code, code.find("three").unwrap()), 3);
    }

    #[test]
    fn it_finds_this_very_file() {
        let files = rust_files_under(&src_root());
        assert!(
            files.iter().any(|(n, _)| n == "source_guard.rs"),
            "the walker must find the tree it exists to walk"
        );
    }
}
