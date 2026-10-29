//! The player's files: `cat` and `ls` over the text files the terminal holds (`Files`).
//!
//! The terminal's file system is flat storage keyed by path; directories exist only as path
//! prefixes. Error messages follow GNU coreutils.

use crate::manifests::{normalize_path, Files};

/// A path as the file map keys it: no leading `./`, no trailing `/`; the current directory is "".
fn key(path: &str) -> String {
    let path = normalize_path(path);
    let trimmed = path.trim_end_matches('/');
    if trimmed == "." {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// Whether `dir` is a directory: the current directory, or a prefix of some file's path.
fn is_dir(files: &Files, dir: &str) -> bool {
    dir.is_empty() || files.keys().any(|f| f.starts_with(&format!("{dir}/")))
}

/// The names directly inside `dir`, sorted.
fn children(files: &Files, dir: &str) -> Vec<String> {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let mut names: Vec<String> = files
        .keys()
        .filter_map(|f| f.strip_prefix(&prefix))
        .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// What a built-in printed: bytes for stdout (newlines exact, so pipes carry them) and the
/// error lines that went to stderr.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: Vec<String>,
}

impl Output {
    /// As the terminal shows it: errors first, then stdout without its final newline.
    pub fn display(&self) -> String {
        let out = self.stdout.strip_suffix('\n').unwrap_or(&self.stdout);
        self.stderr
            .iter()
            .map(String::as_str)
            .chain((!self.stdout.is_empty()).then_some(out))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn cat(paths: &[String], files: &Files) -> Output {
    let mut out = Output::default();
    if paths.is_empty() {
        out.stderr
            .push("cat: reading from standard input is not available in this terminal".into());
    }
    for path in paths {
        let k = key(path);
        match files.get(&k) {
            Some(text) => out.stdout.push_str(text),
            None if is_dir(files, &k) => out.stderr.push(format!("cat: {path}: Is a directory")),
            None => out.stderr.push(format!("cat: {path}: No such file or directory")),
        }
    }
    out
}

pub fn ls(paths: &[String], files: &Files) -> Output {
    let mut out = Output::default();
    if paths.is_empty() {
        let names = children(files, "");
        out.stdout = names.iter().map(|n| format!("{n}\n")).collect();
        return out;
    }
    let (mut found, mut dirs) = (Vec::new(), Vec::new());
    for path in paths {
        let k = key(path);
        if files.contains_key(&k) {
            found.push(path.clone());
        } else if is_dir(files, &k) {
            dirs.push((path.clone(), children(files, &k)));
        } else {
            out.stderr
                .push(format!("ls: cannot access '{path}': No such file or directory"));
        }
    }
    let headings = paths.len() > 1;
    let mut sections = Vec::new();
    if !found.is_empty() {
        sections.push(found.join("\n"));
    }
    for (path, names) in dirs {
        let listing = names.join("\n");
        sections.push(if headings {
            format!("{path}:\n{listing}").trim_end().to_string()
        } else {
            listing
        });
    }
    let text = sections.join(if headings { "\n\n" } else { "\n" });
    if !text.trim().is_empty() {
        out.stdout = format!("{}\n", text.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Files {
        [
            ("notes.txt", "the butler did it\n"),
            ("case/web.yaml", "kind: Pod\n"),
            ("case/db/pg.yaml", "kind: Pod\n"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ls_lists_directories_by_prefix() {
        let ls = |a: &[&str]| super::ls(&args(a), &files()).display();
        assert_eq!(ls(&[]), "case\nnotes.txt");
        assert_eq!(ls(&["case"]), "db\nweb.yaml");
        assert_eq!(ls(&["./case/"]), "db\nweb.yaml");
        assert_eq!(ls(&["."]), "case\nnotes.txt");
        assert_eq!(ls(&["notes.txt"]), "notes.txt");
        assert_eq!(super::ls(&[], &Files::new()), Output::default());
        assert_eq!(super::ls(&args(&["case"]), &files()).stdout, "db\nweb.yaml\n");
    }

    #[test]
    fn ls_with_several_operands_labels_directories() {
        assert_eq!(
            super::ls(&args(&["nope", "notes.txt", "case/db", "case"]), &files()).display(),
            "ls: cannot access 'nope': No such file or directory\nnotes.txt\n\ncase/db:\npg.yaml\n\ncase:\ndb\nweb.yaml"
        );
        assert_eq!(
            super::ls(&args(&["nope"]), &files()).display(),
            "ls: cannot access 'nope': No such file or directory"
        );
    }

    #[test]
    fn cat_keeps_file_bytes_and_reports_each_failure_on_stderr() {
        let out = super::cat(&args(&["notes.txt"]), &files());
        assert_eq!(out.stdout, "the butler did it\n");
        assert_eq!(out.display(), "the butler did it");

        let out = super::cat(&args(&["./notes.txt", "case", "gone"]), &files());
        assert_eq!(out.stdout, "the butler did it\n");
        assert_eq!(
            out.stderr,
            ["cat: case: Is a directory", "cat: gone: No such file or directory"]
        );
        assert_eq!(
            super::cat(&[], &files()).display(),
            "cat: reading from standard input is not available in this terminal"
        );
    }
}
