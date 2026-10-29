//! The terminal's shell: a line is a pipeline of commands (`kubectl`, `cat`, `ls`, `echo`,
//! `base64`, `grep`, `head`) joined by `|`, optionally ending in `> file` or `>> file`.
//!
//! Stages run left to right and pass their stdout on as the next stage's stdin, newlines
//! included (so `echo pass | base64` and `echo -n pass | base64` differ, as they do for real).
//! Errors go to stderr: shown, never piped. Anything else a real shell would do (`;`, `&&`,
//! `<`, `$VAR`, `$(...)`, backticks) is refused by name rather than half-working.

use regex::RegexBuilder;

use crate::controllers::Env;
use crate::exec::execute;
use crate::fs::{self, Output};
use crate::judge::{is_error_output, Touched};
use crate::manifests::{normalize_path, Files};
use crate::parser::{parse, shell_split, Command, OutputFormat, ParseError};

/// A `kubectl`/`cat`/`ls` command a line ran, for the judge.
pub struct Executed {
    pub command: Command,
    pub touched: Vec<Touched>,
    /// What it printed (stdout, then stderr).
    pub output: String,
}

/// What a line printed and which judged commands it ran.
pub struct Line {
    pub output: String,
    pub executed: Vec<Executed>,
}

struct Redirect {
    target: String,
    append: bool,
}

struct Parsed {
    stages: Vec<String>,
    redirect: Option<Redirect>,
}

const UNAVAILABLE: &str = "is not available in this terminal";

fn unsupported(what: &str) -> String {
    format!("error: {what} {UNAVAILABLE}")
}

/// Split a line into pipeline stages (raw text, quotes intact) and a trailing redirection.
fn scan(input: &str) -> Result<Parsed, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut stages: Vec<String> = vec![String::new()];
    let mut redirect: Option<Redirect> = None;
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let stage = stages.last_mut().expect("at least one stage");
        match quote {
            Some('\'') => {
                stage.push(c);
                if c == '\'' {
                    quote = None;
                }
            }
            Some(q) => {
                // Inside double quotes `$` and backticks still substitute.
                if c == '$'
                    && chars
                        .get(i + 1)
                        .is_some_and(|n| n.is_alphanumeric() || "_{(?$".contains(*n))
                {
                    return Err(unsupported("variable and command substitution"));
                }
                if c == '`' {
                    return Err(unsupported("command substitution"));
                }
                stage.push(c);
                if c == '\\' {
                    if let Some(next) = chars.get(i + 1) {
                        stage.push(*next);
                        i += 1;
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    stage.push(c);
                }
                '\\' => {
                    stage.push(c);
                    if let Some(next) = chars.get(i + 1) {
                        stage.push(*next);
                        i += 1;
                    }
                }
                '$' if chars
                    .get(i + 1)
                    .is_some_and(|n| n.is_alphanumeric() || "_{(?$".contains(*n)) =>
                {
                    return Err(unsupported("variable and command substitution"));
                }
                '`' => return Err(unsupported("command substitution")),
                ';' | '&' | '<' | '(' | ')' => return Err(unsupported(&format!("`{c}`"))),
                '|' => {
                    if redirect.is_some() {
                        return Err(unsupported("a redirection before a pipe"));
                    }
                    stages.push(String::new());
                }
                '>' => {
                    if redirect.is_some() {
                        return Err(unsupported("more than one redirection"));
                    }
                    let append = chars.get(i + 1) == Some(&'>');
                    i += if append { 2 } else { 1 };
                    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                        i += 1;
                    }
                    let mut word = String::new();
                    let mut word_quote: Option<char> = None;
                    while let Some(&w) = chars.get(i) {
                        if word_quote.is_none() && (w.is_whitespace() || "|><;&".contains(w)) {
                            break;
                        }
                        match word_quote {
                            Some(q) if w == q => word_quote = None,
                            None if w == '\'' || w == '"' => word_quote = Some(w),
                            _ => {}
                        }
                        word.push(w);
                        i += 1;
                    }
                    let words = shell_split(&word).map_err(|e| e.to_string())?;
                    match words.as_slice() {
                        [target] => {
                            redirect = Some(Redirect {
                                target: target.clone(),
                                append,
                            })
                        }
                        [] => return Err("syntax error near unexpected token `newline'".into()),
                        _ => return Err("error: ambiguous redirect".into()),
                    }
                    stage.push(' ');
                    continue;
                }
                _ => stage.push(c),
            },
        }
        i += 1;
    }
    if quote.is_some() {
        return Err(ParseError::UnterminatedQuote.to_string());
    }
    if stages.len() > 1 {
        if let Some(position) = stages.iter().position(|s| s.trim().is_empty()) {
            let token = if position + 1 == stages.len() { "newline" } else { "|" };
            return Err(format!("syntax error near unexpected token `{token}'"));
        }
    }
    Ok(Parsed {
        stages: stages.into_iter().map(|s| s.trim().to_string()).collect(),
        redirect,
    })
}

/// Run one line.
pub fn run(line: &str, env: &Env<'_>, files: &mut Files) -> Line {
    let mut executed = Vec::new();
    let fail = |message: String| Line {
        output: message,
        executed: Vec::new(),
    };
    if line.trim().is_empty() {
        return fail(ParseError::Empty.to_string());
    }
    let parsed = match scan(line) {
        Ok(parsed) => parsed,
        Err(message) => return fail(message),
    };

    let mut stdin = String::new();
    let mut stderr: Vec<String> = Vec::new();
    for (index, stage) in parsed.stages.iter().enumerate() {
        let words = match shell_split(stage) {
            Ok(words) => words,
            Err(e) => {
                stderr.push(e.to_string());
                stdin = String::new();
                continue;
            }
        };
        let result = match words.first().map(String::as_str) {
            Some("echo") => Output {
                stdout: echo(&words[1..]),
                stderr: Vec::new(),
            },
            Some("base64") => base64_command(&words[1..], &stdin, files),
            Some("grep") => grep(&words[1..], &stdin, files),
            Some("head") => head(&words[1..], &stdin, files),
            _ => match parse(stage) {
                Err(e) => Output {
                    stdout: String::new(),
                    stderr: vec![e.to_string()],
                },
                // `cat` after a pipe with no files copies its input.
                Ok(Command::Cat(args)) if index > 0 && args.paths.is_empty() => Output {
                    stdout: stdin.clone(),
                    stderr: Vec::new(),
                },
                Ok(command) => run_command(command, env, files, &mut executed),
            },
        };
        stderr.extend(result.stderr);
        stdin = result.stdout;
    }

    let mut output = stdin;
    if let Some(redirect) = parsed.redirect {
        let path = normalize_path(&redirect.target);
        let prefix = format!("{path}/");
        if path.is_empty() || files.keys().any(|f| f.starts_with(&prefix)) {
            stderr.push(format!("error: {}: Is a directory", redirect.target));
        } else {
            let text = files.entry(path).or_default();
            if !redirect.append {
                text.clear();
            }
            text.push_str(&output);
        }
        output = String::new();
    }
    Line {
        output: Output { stdout: output, stderr }.display(),
        executed,
    }
}

/// `-o jsonpath` prints exactly what its template says, with no newline added after it.
fn exact_bytes(command: &Command) -> bool {
    matches!(command, Command::Get(args) if matches!(args.output, OutputFormat::JsonPath(_)))
}

/// A judged command. Its stdout continues down the pipe unless the whole thing was an error.
fn run_command(command: Command, env: &Env<'_>, files: &Files, executed: &mut Vec<Executed>) -> Output {
    let result = match &command {
        Command::Cat(args) => fs::cat(&args.paths, files),
        Command::Ls(args) => fs::ls(&args.paths, files),
        other => {
            let (text, touched) = execute(other, env, files);
            let out = if is_error_output(&text) {
                Output {
                    stdout: String::new(),
                    stderr: vec![text.clone()],
                }
            } else if text.is_empty() {
                Output::default()
            } else if exact_bytes(&command) {
                Output {
                    stdout: text.clone(),
                    stderr: Vec::new(),
                }
            } else {
                Output {
                    stdout: format!("{text}\n"),
                    stderr: Vec::new(),
                }
            };
            executed.push(Executed {
                command,
                touched,
                output: text,
            });
            return out;
        }
    };
    let mut seen = result.stdout.strip_suffix('\n').unwrap_or(&result.stdout).to_string();
    for line in &result.stderr {
        if !seen.is_empty() {
            seen.push('\n');
        }
        seen.push_str(line);
    }
    executed.push(Executed {
        command,
        touched: Vec::new(),
        output: seen,
    });
    result
}

// ---- echo -----------------------------------------------------------------------------------

fn echo(args: &[String]) -> String {
    let (mut newline, mut escapes) = (true, false);
    let mut rest = args;
    while let Some(first) = rest.first() {
        let flags = first.strip_prefix('-').unwrap_or("");
        if flags.is_empty() || !flags.chars().all(|c| "neE".contains(c)) {
            break;
        }
        for flag in flags.chars() {
            match flag {
                'n' => newline = false,
                'e' => escapes = true,
                _ => escapes = false,
            }
        }
        rest = &rest[1..];
    }
    let mut text = rest.join(" ");
    if escapes {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('c') => {
                    newline = false;
                    break;
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        text = out;
    }
    if newline {
        text.push('\n');
    }
    text
}

// ---- operands shared by the text filters ----------------------------------------------------

fn fail(command: &str, message: String) -> Output {
    Output {
        stdout: String::new(),
        stderr: vec![format!("{command}: {message}")],
    }
}

fn bad_option(command: &str, arg: &str) -> Output {
    match arg.strip_prefix("--") {
        Some(long) => fail(command, format!("unrecognized option '--{long}'")),
        None => fail(
            command,
            format!("invalid option -- '{}'", arg.chars().nth(1).unwrap_or('-')),
        ),
    }
}

/// The text to filter: stdin when there are no operands, else each file (a missing one is an
/// error on stderr, like coreutils; the others are still read).
fn inputs(
    command: &str,
    operands: &[String],
    stdin: &str,
    files: &Files,
    errors: &mut Vec<String>,
) -> Vec<(String, String)> {
    if operands.is_empty() {
        return vec![("(standard input)".into(), stdin.to_string())];
    }
    operands
        .iter()
        .filter_map(|path| match files.get(&normalize_path(path)) {
            Some(text) => Some((path.clone(), text.clone())),
            None => {
                let is_dir = files
                    .keys()
                    .any(|f| f.starts_with(&format!("{}/", normalize_path(path))));
                let why = if is_dir {
                    "Is a directory"
                } else {
                    "No such file or directory"
                };
                errors.push(format!("{command}: {path}: {why}"));
                None
            }
        })
        .collect()
}

// ---- base64 ---------------------------------------------------------------------------------

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let cleaned: Vec<u8> = text.bytes().filter(|b| *b != b'\n' && *b != b'\r').collect();
    if !cleaned.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    let blocks = cleaned.len() / 4;
    for (index, block) in cleaned.chunks(4).enumerate() {
        let padding = block.iter().rev().take_while(|b| **b == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != blocks) {
            return None;
        }
        let mut n = 0u32;
        for (i, b) in block.iter().enumerate() {
            let value = if i >= 4 - padding {
                0
            } else {
                ALPHABET.iter().position(|a| a == b)? as u32
            };
            n |= value << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..4 - padding]);
    }
    Some(out)
}

fn base64_command(args: &[String], stdin: &str, files: &Files) -> Output {
    let (mut decode, mut wrap) = (false, 76usize);
    let mut operands = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value_of = |inline: &str, iter: &mut std::slice::Iter<'_, String>| {
            if inline.is_empty() {
                iter.next().cloned()
            } else {
                Some(inline.to_string())
            }
        };
        let width = |value: Option<String>| value.and_then(|v| v.parse::<usize>().ok());
        match arg.as_str() {
            "-d" | "-D" | "--decode" => decode = true,
            "-w" => match width(value_of("", &mut iter)) {
                Some(w) => wrap = w,
                None => return fail("base64", "invalid wrap size".into()),
            },
            "--" => operands.extend(iter.by_ref().cloned()),
            a if a.starts_with("--wrap=") => match width(Some(a["--wrap=".len()..].to_string())) {
                Some(w) => wrap = w,
                None => return fail("base64", format!("invalid wrap size: '{}'", &a["--wrap=".len()..])),
            },
            a if a.starts_with("-w") && !a.starts_with("--") => match width(Some(a[2..].to_string())) {
                Some(w) => wrap = w,
                None => return fail("base64", format!("invalid wrap size: '{}'", &a[2..])),
            },
            a if a.starts_with('-') && a.len() > 1 => return bad_option("base64", a),
            _ => operands.push(arg.clone()),
        }
    }
    if operands.len() > 1 {
        return fail("base64", format!("extra operand '{}'", operands[1]));
    }
    let mut errors = Vec::new();
    let data = inputs("base64", &operands, stdin, files, &mut errors);
    let Some((_, text)) = data.first() else {
        return Output {
            stdout: String::new(),
            stderr: errors,
        };
    };
    if decode {
        return match base64_decode(text) {
            Some(bytes) => Output {
                stdout: String::from_utf8_lossy(&bytes).into_owned(),
                stderr: errors,
            },
            None => fail("base64", "invalid input".into()),
        };
    }
    let encoded = base64_encode(text.as_bytes());
    let stdout = match (encoded.is_empty(), wrap) {
        (true, _) => String::new(),
        (false, 0) => encoded,
        (false, w) => {
            let lines: Vec<&str> = encoded
                .as_bytes()
                .chunks(w)
                .map(|c| std::str::from_utf8(c).unwrap_or(""))
                .collect();
            format!("{}\n", lines.join("\n"))
        }
    };
    Output { stdout, stderr: errors }
}

// ---- grep -----------------------------------------------------------------------------------

/// Turn a POSIX basic regular expression into the regex crate's syntax: `\( \) \| \+ \? \{ \}`
/// are the operators and the bare characters are literals; `\< \>` are word edges.
fn basic_to_regex(pattern: &str) -> Result<String, &'static str> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => match chars.get(i + 1) {
                Some(&n) if "(){}|+?".contains(n) => {
                    out.push(n);
                    i += 1;
                }
                Some('<') | Some('>') => {
                    out.push_str("\\b");
                    i += 1;
                }
                Some(&n) => {
                    out.push('\\');
                    out.push(n);
                    i += 1;
                }
                None => out.push_str("\\\\"),
            },
            '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                out.push('\\');
                out.push(c);
            }
            '*' if out.is_empty() || out.ends_with('^') && out.len() == 1 => out.push_str("\\*"),
            '[' => {
                // A bracket expression is copied as is, up to its closing `]`.
                out.push('[');
                i += 1;
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '[' && chars.get(i + 1) != Some(&':') {
                        out.push_str("\\[");
                    } else if chars[i] == '\\' {
                        out.push_str("\\\\");
                    } else {
                        out.push(chars[i]);
                    }
                    i += 1;
                }
                if i >= chars.len() {
                    return Err("Unmatched [, [^, [:, [., or [=");
                }
                out.push(']');
            }
            _ => out.push(c),
        }
        i += 1;
    }
    Ok(out)
}

fn grep(args: &[String], stdin: &str, files: &Files) -> Output {
    let (mut ignore_case, mut invert, mut count, mut numbers) = (false, false, false, false);
    let (mut extended, mut fixed) = (false, false);
    let mut patterns: Vec<String> = Vec::new();
    let mut operands: Vec<String> = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--" => operands.extend(iter.by_ref().cloned()),
            "--ignore-case" => ignore_case = true,
            "--invert-match" => invert = true,
            "--count" => count = true,
            "--line-number" => numbers = true,
            "--extended-regexp" => extended = true,
            "--fixed-strings" => fixed = true,
            a if a.starts_with("--regexp=") => patterns.push(a["--regexp=".len()..].to_string()),
            a if a.starts_with("--") => return bad_option("grep", a),
            a if a.starts_with('-') && a.len() > 1 => {
                let mut flags = a[1..].chars();
                while let Some(flag) = flags.next() {
                    match flag {
                        'i' | 'y' => ignore_case = true,
                        'v' => invert = true,
                        'c' => count = true,
                        'n' => numbers = true,
                        'E' => extended = true,
                        'F' => fixed = true,
                        'G' => {}
                        'e' => {
                            let rest: String = flags.by_ref().collect();
                            match if rest.is_empty() {
                                iter.next().cloned()
                            } else {
                                Some(rest)
                            } {
                                Some(p) => patterns.push(p),
                                None => return fail("grep", "option requires an argument -- 'e'".into()),
                            }
                        }
                        _ => return bad_option("grep", a),
                    }
                }
            }
            _ => operands.push(arg.clone()),
        }
    }
    if patterns.is_empty() {
        if operands.is_empty() {
            return fail("grep", "Usage: grep [OPTION]... PATTERNS [FILE]...".into());
        }
        patterns.push(operands.remove(0));
    }
    let mut alternatives = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split('\n')) {
        alternatives.push(if fixed {
            regex::escape(p)
        } else if extended {
            p.to_string()
        } else {
            match basic_to_regex(p) {
                Ok(translated) => translated,
                Err(message) => return fail("grep", message.into()),
            }
        });
    }
    let source = alternatives.join("|");
    let regex = match RegexBuilder::new(&source).case_insensitive(ignore_case).build() {
        Ok(regex) => regex,
        Err(_) => return fail("grep", "Invalid regular expression".into()),
    };

    let mut errors = Vec::new();
    let data = inputs("grep", &operands, stdin, files, &mut errors);
    let prefix_names = operands.len() > 1;
    let mut stdout = String::new();
    for (name, text) in &data {
        let mut matched = 0;
        for (number, line) in text.split_terminator('\n').enumerate() {
            if regex.is_match(line) == invert {
                continue;
            }
            matched += 1;
            if !count {
                if prefix_names {
                    stdout.push_str(&format!("{name}:"));
                }
                if numbers {
                    stdout.push_str(&format!("{}:", number + 1));
                }
                stdout.push_str(line);
                stdout.push('\n');
            }
        }
        if count {
            if prefix_names {
                stdout.push_str(&format!("{name}:"));
            }
            stdout.push_str(&format!("{matched}\n"));
        }
    }
    Output { stdout, stderr: errors }
}

// ---- head -----------------------------------------------------------------------------------

fn head(args: &[String], stdin: &str, files: &Files) -> Output {
    let mut lines = 10usize;
    let mut operands: Vec<String> = Vec::new();
    let mut iter = args.iter();
    let parse_count = |value: &str| -> Result<usize, Output> {
        if value.starts_with('-') && value.len() > 1 {
            return Err(fail(
                "head",
                "negative line counts are not available in this terminal".into(),
            ));
        }
        value
            .parse::<usize>()
            .map_err(|_| fail("head", format!("invalid number of lines: '{value}'")))
    };
    while let Some(arg) = iter.next() {
        let result = match arg.as_str() {
            "--" => {
                operands.extend(iter.by_ref().cloned());
                continue;
            }
            "-n" | "--lines" => match iter.next() {
                Some(value) => parse_count(value),
                None => {
                    return fail(
                        "head",
                        format!("option requires an argument -- '{}'", arg.trim_start_matches('-')),
                    )
                }
            },
            a if a.starts_with("--lines=") => parse_count(&a["--lines=".len()..]),
            a if a.starts_with("-n") => parse_count(&a[2..]),
            a if a.len() > 1 && a[1..].chars().all(|c| c.is_ascii_digit()) && a.starts_with('-') => {
                parse_count(&a[1..])
            }
            a if a.starts_with('-') && a.len() > 1 => return bad_option("head", a),
            _ => {
                operands.push(arg.clone());
                continue;
            }
        };
        match result {
            Ok(n) => lines = n,
            Err(out) => return out,
        }
    }
    let mut errors = Vec::new();
    let data = inputs("head", &operands, stdin, files, &mut errors);
    let headings = operands.len() > 1;
    let mut stdout = String::new();
    for (index, (name, text)) in data.iter().enumerate() {
        if headings {
            if index > 0 {
                stdout.push('\n');
            }
            stdout.push_str(&format!("==> {name} <==\n"));
        }
        stdout.extend(text.split_inclusive('\n').take(lines));
    }
    Output { stdout, stderr: errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages(line: &str) -> Vec<String> {
        scan(line).unwrap().stages
    }

    #[test]
    fn scan_splits_on_unquoted_pipes_only() {
        assert_eq!(
            stages("kubectl get pods | grep 'a|b' | head -n 1"),
            ["kubectl get pods", "grep 'a|b'", "head -n 1"]
        );
        assert_eq!(stages(r#"echo "a | b" \| c"#), [r#"echo "a | b" \| c"#]);
        assert_eq!(stages("echo '$HOME `x` ; &'"), ["echo '$HOME `x` ; &'"]);
    }

    #[test]
    fn scan_finds_the_redirect_and_removes_it_from_the_stage() {
        let parsed = scan("echo hi >> out/a.txt").unwrap();
        assert_eq!(parsed.stages, ["echo hi"]);
        let redirect = parsed.redirect.unwrap();
        assert_eq!((redirect.target.as_str(), redirect.append), ("out/a.txt", true));
        let parsed = scan("echo 'a>b' > 'my file.txt'").unwrap();
        assert_eq!(parsed.stages, ["echo 'a>b'"]);
        assert_eq!(parsed.redirect.unwrap().target, "my file.txt");
    }

    #[test]
    fn scan_refuses_what_it_cannot_do_by_name() {
        let err = |l: &str| scan(l).err().unwrap();
        assert_eq!(
            err("echo $HOME"),
            "error: variable and command substitution is not available in this terminal"
        );
        assert_eq!(
            err("echo \"$(date)\""),
            "error: variable and command substitution is not available in this terminal"
        );
        assert_eq!(
            err("echo `date`"),
            "error: command substitution is not available in this terminal"
        );
        assert_eq!(err("echo a; echo b"), "error: `;` is not available in this terminal");
        assert_eq!(err("echo a && echo b"), "error: `&` is not available in this terminal");
        assert_eq!(err("cat < f"), "error: `<` is not available in this terminal");
        assert_eq!(
            err("echo a > f | cat"),
            "error: a redirection before a pipe is not available in this terminal"
        );
        assert_eq!(
            err("echo a > f > g"),
            "error: more than one redirection is not available in this terminal"
        );
        assert_eq!(err("echo a >"), "syntax error near unexpected token `newline'");
        assert_eq!(err("echo a |"), "syntax error near unexpected token `newline'");
        assert_eq!(err("| cat"), "syntax error near unexpected token `|'");
        assert_eq!(err("echo 'a"), "error: unterminated quoted string");
        // A lone dollar sign is just a character.
        assert!(scan("echo cost: 5$ and $").is_ok());
    }

    #[test]
    fn echo_flags() {
        let e = |a: &[&str]| echo(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(e(&["pass"]), "pass\n");
        assert_eq!(e(&["-n", "pass"]), "pass");
        assert_eq!(e(&["-n", "-n", "a", "-n"]), "a -n");
        assert_eq!(e(&["-e", "a\\nb\\tc"]), "a\nb\tc\n");
        assert_eq!(e(&["-ne", "a\\n"]), "a\n");
        assert_eq!(e(&["a\\nb"]), "a\\nb\n");
        assert_eq!(e(&["-x", "a"]), "-x a\n");
        assert_eq!(e(&[]), "\n");
    }

    #[test]
    fn base64_roundtrip_and_known_values() {
        assert_eq!(base64_encode(b"pass"), "cGFzcw==");
        assert_eq!(base64_encode(b"pass\n"), "cGFzcwo=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        for text in [
            "",
            "a",
            "ab",
            "abc",
            "abcd",
            "postgres://payments@db:5432/payments",
            "ünï\n",
        ] {
            assert_eq!(base64_decode(&base64_encode(text.as_bytes())).unwrap(), text.as_bytes());
        }
        assert!(base64_decode("cGFzcw=").is_none());
        assert!(base64_decode("cGFz*w==").is_none());
        assert!(base64_decode("cGFz=GFz").is_none());
    }

    fn files() -> Files {
        Files::from([("log.txt".to_string(), "alpha\nBeta\ngamma\nbeta two\n".to_string())])
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn base64_command_wraps_and_decodes() {
        let b = |a: &[&str], input: &str| base64_command(&args(a), input, &files());
        assert_eq!(b(&[], "pass").stdout, "cGFzcw==\n");
        assert_eq!(b(&["-w", "0"], "pass").stdout, "cGFzcw==");
        assert_eq!(b(&["--wrap=4"], "pass").stdout, "cGFz\ncw==\n");
        assert_eq!(b(&["-d"], "cGFzcw==\n").stdout, "pass");
        assert_eq!(b(&["--decode"], "cGFz\ncw==").stdout, "pass");
        assert_eq!(b(&["-d"], "!!!").stderr, ["base64: invalid input"]);
        assert_eq!(b(&["-x"], "").stderr, ["base64: invalid option -- 'x'"]);
        assert_eq!(b(&["log.txt"], "").stdout, "YWxwaGEKQmV0YQpnYW1tYQpiZXRhIHR3bwo=\n");
        assert_eq!(b(&["nope"], "").stderr, ["base64: nope: No such file or directory"]);
    }

    #[test]
    fn grep_matches_lines() {
        let g = |a: &[&str], input: &str| grep(&args(a), input, &files());
        let text = "alpha\nBeta\ngamma\nbeta two\n";
        assert_eq!(g(&["beta"], text).stdout, "beta two\n");
        assert_eq!(g(&["-i", "beta"], text).stdout, "Beta\nbeta two\n");
        assert_eq!(g(&["-v", "a$"], text).stdout, "beta two\n");
        assert_eq!(g(&["-c", "a"], text).stdout, "4\n");
        assert_eq!(g(&["-n", "^[bg]"], text).stdout, "3:gamma\n4:beta two\n");
        assert_eq!(
            g(&["-in", "-e", "gamma", "-e", "^b"], text).stdout,
            "2:Beta\n3:gamma\n4:beta two\n"
        );
        assert_eq!(g(&["-F", "t.o"], "a t.o b\nt-o\n").stdout, "a t.o b\n");
        assert_eq!(g(&["beta", "log.txt"], "").stdout, "beta two\n");
        assert_eq!(
            g(&["a", "log.txt", "log.txt"], "").stdout.lines().next(),
            Some("log.txt:alpha")
        );
        assert_eq!(
            g(&["a", "nope.txt"], "").stderr,
            ["grep: nope.txt: No such file or directory"]
        );
    }

    #[test]
    fn grep_basic_vs_extended_syntax() {
        let g = |a: &[&str], input: &str| grep(&args(a), input, &files()).stdout;
        // In basic syntax these are plain characters; escaped, they are operators.
        assert_eq!(g(&["a+b"], "a+b\naab\n"), "a+b\n");
        assert_eq!(g(&["a\\+b"], "a+b\naab\n"), "aab\n");
        assert_eq!(g(&["x|y"], "x|y\nx\n"), "x|y\n");
        assert_eq!(g(&["x\\|y"], "x\ny\nz\n"), "x\ny\n");
        assert_eq!(g(&["-E", "x|y"], "x\ny\nz\n"), "x\ny\n");
        assert_eq!(g(&["(a)"], "(a)\na\n"), "(a)\n");
        assert_eq!(g(&["*a"], "*a\na\n"), "*a\n");
        assert_eq!(g(&["[[:digit:]]\\+"], "a1\nb\n"), "a1\n");
        assert_eq!(g(&["\\<ab\\>"], "ab\nabc\n"), "ab\n");
    }

    #[test]
    fn grep_errors() {
        let g = |a: &[&str]| grep(&args(a), "", &files()).stderr;
        assert_eq!(g(&["-z", "x"]), ["grep: invalid option -- 'z'"]);
        assert_eq!(g(&["--color", "x"]), ["grep: unrecognized option '--color'"]);
        assert_eq!(g(&["[a"]), ["grep: Unmatched [, [^, [:, [., or [="]);
        assert_eq!(g(&["-E", "(a"]), ["grep: Invalid regular expression"]);
        assert_eq!(g(&[]), ["grep: Usage: grep [OPTION]... PATTERNS [FILE]..."]);
        assert_eq!(g(&["-e"]), ["grep: option requires an argument -- 'e'"]);
    }

    #[test]
    fn head_takes_lines_and_keeps_bytes() {
        let h = |a: &[&str], input: &str| head(&args(a), input, &files());
        let text: String = (1..=12).map(|n| format!("{n}\n")).collect();
        assert_eq!(h(&[], &text).stdout.lines().count(), 10);
        assert_eq!(h(&["-n", "2"], &text).stdout, "1\n2\n");
        assert_eq!(h(&["-n2"], &text).stdout, "1\n2\n");
        assert_eq!(h(&["-3"], &text).stdout, "1\n2\n3\n");
        assert_eq!(h(&["--lines=1"], &text).stdout, "1\n");
        assert_eq!(h(&["-n", "5"], "no newline").stdout, "no newline");
        assert_eq!(h(&["-n", "1", "log.txt"], "").stdout, "alpha\n");
        assert_eq!(
            h(&["-n", "1", "log.txt", "log.txt"], "").stdout,
            "==> log.txt <==\nalpha\n\n==> log.txt <==\nalpha\n"
        );
        assert_eq!(h(&["-n", "x"], "").stderr, ["head: invalid number of lines: 'x'"]);
        assert_eq!(
            h(&["-n", "-2"], "").stderr,
            ["head: negative line counts are not available in this terminal"]
        );
        assert_eq!(h(&["-c", "2"], "").stderr, ["head: invalid option -- 'c'"]);
    }
}
