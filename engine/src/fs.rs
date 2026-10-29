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

pub fn cat(paths: &[String], files: &Files) -> String {
    if paths.is_empty() {
        return "cat: reading from standard input is not available in this terminal".into();
    }
    let mut out = Vec::new();
    for path in paths {
        let k = key(path);
        match files.get(&k) {
            Some(text) => out.push(text.trim_end_matches('\n').to_string()),
            None if is_dir(files, &k) => out.push(format!("cat: {path}: Is a directory")),
            None => out.push(format!("cat: {path}: No such file or directory")),
        }
    }
    out.join("\n")
}

pub fn ls(paths: &[String], files: &Files) -> String {
    if paths.is_empty() {
        return children(files, "").join("\n");
    }
    let (mut found, mut dirs, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    for path in paths {
        let k = key(path);
        if files.contains_key(&k) {
            found.push(path.clone());
        } else if is_dir(files, &k) {
            dirs.push((path.clone(), children(files, &k)));
        } else {
            errors.push(format!("ls: cannot access '{path}': No such file or directory"));
        }
    }
    let headings = paths.len() > 1;
    let mut sections = errors;
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
    sections
        .join(if headings { "\n\n" } else { "\n" })
        .trim_end()
        .to_string()
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
        assert_eq!(ls(&[], &files()), "case\nnotes.txt");
        assert_eq!(ls(&args(&["case"]), &files()), "db\nweb.yaml");
        assert_eq!(ls(&args(&["./case/"]), &files()), "db\nweb.yaml");
        assert_eq!(ls(&args(&["."]), &files()), "case\nnotes.txt");
        assert_eq!(ls(&args(&["notes.txt"]), &files()), "notes.txt");
        assert_eq!(ls(&[], &Files::new()), "");
    }

    #[test]
    fn ls_with_several_operands_labels_directories() {
        assert_eq!(
            ls(&args(&["nope", "notes.txt", "case/db", "case"]), &files()),
            "ls: cannot access 'nope': No such file or directory\n\nnotes.txt\n\ncase/db:\npg.yaml\n\ncase:\ndb\nweb.yaml"
        );
        assert_eq!(
            ls(&args(&["nope"]), &files()),
            "ls: cannot access 'nope': No such file or directory"
        );
    }

    #[test]
    fn cat_prints_files_and_reports_each_failure() {
        assert_eq!(cat(&args(&["notes.txt"]), &files()), "the butler did it");
        assert_eq!(
            cat(&args(&["./notes.txt", "case", "gone"]), &files()),
            "the butler did it\ncat: case: Is a directory\ncat: gone: No such file or directory"
        );
        assert_eq!(
            cat(&[], &files()),
            "cat: reading from standard input is not available in this terminal"
        );
    }
}
