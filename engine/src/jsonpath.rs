//! `kubectl get -o jsonpath=...`: the template language kubectl uses, minus recursive descent.
//!
//! A template is text and `{...}` actions: a path (`{.metadata.name}`, `{.items[*].spec.nodeName}`,
//! `{.spec.containers[0].image}`, `{.items[?(@.kind=="Pod")].metadata.name}`), a quoted literal
//! (`{"\n"}`), or `{range PATH}...{end}` to repeat a body for each match. Several matches of
//! one path print separated by spaces. A path that matches nothing prints nothing.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Field(String),
    Wildcard,
    Index(i64),
    Slice(Option<i64>, Option<i64>),
    Filter(Filter),
}

#[derive(Debug, Clone, PartialEq)]
struct Filter {
    path: Vec<Step>,
    test: Option<(Op, Value)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Path(Vec<Step>),
    Range(Vec<Step>, Vec<Node>),
}

/// Render `template` against `root`. `Err` is the message to print after `error: `.
pub fn render(template: &str, root: &Value) -> Result<String, String> {
    let nodes = parse(template).map_err(|why| format!("error parsing jsonpath {template}, {why}"))?;
    let mut out = String::new();
    emit(&nodes, root, &mut out);
    Ok(out)
}

// ---- parsing --------------------------------------------------------------------------------

fn parse(template: &str) -> Result<Vec<Node>, String> {
    // Stack of (range path, nodes so far); the bottom entry is the template itself.
    let mut stack: Vec<(Option<Vec<Step>>, Vec<Node>)> = vec![(None, Vec::new())];
    let mut text = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            text.push(c);
            continue;
        }
        if !text.is_empty() {
            stack
                .last_mut()
                .expect("stack")
                .1
                .push(Node::Text(std::mem::take(&mut text)));
        }
        let mut action = String::new();
        let mut quote: Option<char> = None;
        let mut closed = false;
        while let Some(a) = chars.next() {
            match quote {
                Some(q) => {
                    action.push(a);
                    if a == '\\' {
                        action.extend(chars.next());
                    } else if a == q {
                        quote = None;
                    }
                }
                None if a == '}' => {
                    closed = true;
                    break;
                }
                None => {
                    if a == '"' || a == '\'' {
                        quote = Some(a);
                    }
                    action.push(a);
                }
            }
        }
        if !closed {
            return Err("unclosed action".into());
        }
        let action = action.trim();
        if action == "end" {
            if stack.len() == 1 {
                return Err("unexpected {end}".into());
            }
            let (path, body) = stack.pop().expect("checked");
            stack
                .last_mut()
                .expect("stack")
                .1
                .push(Node::Range(path.unwrap_or_default(), body));
        } else if let Some(path) = action.strip_prefix("range ") {
            stack.push((Some(parse_path(path.trim())?), Vec::new()));
        } else if let Some(literal) = quoted(action)? {
            stack.last_mut().expect("stack").1.push(Node::Text(literal));
        } else {
            stack.last_mut().expect("stack").1.push(Node::Path(parse_path(action)?));
        }
    }
    if stack.len() > 1 {
        return Err("range action missing end".into());
    }
    if !text.is_empty() {
        stack[0].1.push(Node::Text(text));
    }
    Ok(stack.remove(0).1)
}

/// A `"..."` or `'...'` literal with `\n`, `\t`, `\\` and escaped quotes resolved.
fn quoted(action: &str) -> Result<Option<String>, String> {
    let Some(q) = action.chars().next().filter(|c| *c == '"' || *c == '\'') else {
        return Ok(None);
    };
    let inner = action[1..]
        .strip_suffix(q)
        .ok_or_else(|| "unterminated quoted string".to_string())?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Ok(Some(out))
}

fn parse_path(text: &str) -> Result<Vec<Step>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut steps = Vec::new();
    let mut i = 0;
    if matches!(chars.first(), Some('$') | Some('@')) {
        i = 1;
    }
    while i < chars.len() {
        match chars[i] {
            '.' => {
                if chars.get(i + 1) == Some(&'.') {
                    return Err("recursive descent (..) is not supported".into());
                }
                i += 1;
                let start = i;
                while i < chars.len() && !matches!(chars[i], '.' | '[') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                match name.as_str() {
                    "" => return Err(format!("empty field name in {text}")),
                    "*" => steps.push(Step::Wildcard),
                    _ => steps.push(Step::Field(name)),
                }
            }
            '[' => {
                let mut depth = 0;
                let mut quote: Option<char> = None;
                let start = i + 1;
                let mut end = None;
                for (j, &c) in chars.iter().enumerate().skip(i) {
                    match quote {
                        Some(q) if c == q => quote = None,
                        Some(_) => {}
                        None => match c {
                            '\'' | '"' => quote = Some(c),
                            '[' => depth += 1,
                            ']' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = Some(j);
                                    break;
                                }
                            }
                            _ => {}
                        },
                    }
                }
                let end = end.ok_or_else(|| format!("unterminated array subscript in {text}"))?;
                let inner: String = chars[start..end].iter().collect();
                steps.push(subscript(inner.trim())?);
                i = end + 1;
            }
            _ if i == 0 || (i == 1 && matches!(chars[0], '$' | '@')) => {
                // A path without a leading dot: `metadata.name`.
                let start = i;
                while i < chars.len() && !matches!(chars[i], '.' | '[') {
                    i += 1;
                }
                steps.push(Step::Field(chars[start..i].iter().collect()));
            }
            c => return Err(format!("unexpected {c:?} in {text}")),
        }
    }
    Ok(steps)
}

fn subscript(inner: &str) -> Result<Step, String> {
    if inner == "*" {
        return Ok(Step::Wildcard);
    }
    if let Some(filter) = inner.strip_prefix("?(").and_then(|f| f.strip_suffix(')')) {
        return Ok(Step::Filter(parse_filter(filter.trim())?));
    }
    if let Some(key) = quoted(inner)? {
        return Ok(Step::Field(key));
    }
    if let Some((from, to)) = inner.split_once(':') {
        let bound = |b: &str| {
            let b = b.trim();
            if b.is_empty() {
                Ok(None)
            } else {
                b.parse::<i64>()
                    .map(Some)
                    .map_err(|_| format!("invalid slice bound {b:?}"))
            }
        };
        return Ok(Step::Slice(bound(from)?, bound(to)?));
    }
    inner
        .parse::<i64>()
        .map(Step::Index)
        .map_err(|_| format!("invalid array index {inner:?}"))
}

fn parse_filter(text: &str) -> Result<Filter, String> {
    for (token, op) in [
        ("==", Op::Eq),
        ("!=", Op::Ne),
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("<", Op::Lt),
        (">", Op::Gt),
    ] {
        if let Some((left, right)) = text.split_once(token) {
            let right = right.trim();
            let value = match quoted(right)? {
                Some(s) => Value::String(s),
                None => serde_json::from_str(right).map_err(|_| format!("invalid filter value {right:?}"))?,
            };
            return Ok(Filter {
                path: parse_path(left.trim())?,
                test: Some((op, value)),
            });
        }
    }
    Ok(Filter {
        path: parse_path(text)?,
        test: None,
    })
}

// ---- evaluation -----------------------------------------------------------------------------

fn emit(nodes: &[Node], context: &Value, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Path(path) => {
                let shown: Vec<String> = walk(path, context).iter().map(show).collect();
                out.push_str(&shown.join(" "));
            }
            Node::Range(path, body) => {
                for item in walk(path, context) {
                    emit(body, &item, out);
                }
            }
        }
    }
}

fn show(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn walk(path: &[Step], start: &Value) -> Vec<Value> {
    let mut current = vec![start.clone()];
    for step in path {
        let mut next = Vec::new();
        for value in &current {
            match step {
                Step::Field(name) => next.extend(value.get(name).cloned()),
                Step::Wildcard => match value {
                    Value::Array(items) => next.extend(items.iter().cloned()),
                    Value::Object(fields) => next.extend(fields.values().cloned()),
                    _ => {}
                },
                Step::Index(i) => {
                    if let Value::Array(items) = value {
                        let index = if *i < 0 { items.len() as i64 + i } else { *i };
                        next.extend(usize::try_from(index).ok().and_then(|i| items.get(i)).cloned());
                    }
                }
                Step::Slice(from, to) => {
                    if let Value::Array(items) = value {
                        let clamp = |b: i64| {
                            let b = if b < 0 { items.len() as i64 + b } else { b };
                            b.clamp(0, items.len() as i64) as usize
                        };
                        let (from, to) = (from.map_or(0, clamp), to.map_or(items.len(), clamp));
                        next.extend(items.get(from..to.max(from)).unwrap_or_default().iter().cloned());
                    }
                }
                Step::Filter(filter) => {
                    if let Value::Array(items) = value {
                        next.extend(items.iter().filter(|item| passes(filter, item)).cloned());
                    }
                }
            }
        }
        current = next;
    }
    current
}

fn passes(filter: &Filter, item: &Value) -> bool {
    let found = walk(&filter.path, item);
    let Some((op, expected)) = &filter.test else {
        return !found.is_empty();
    };
    found
        .iter()
        .any(|actual| match (op, actual.as_f64().zip(expected.as_f64())) {
            (Op::Eq, _) => actual == expected,
            (Op::Ne, _) => actual != expected,
            (Op::Lt, Some((a, b))) => a < b,
            (Op::Gt, Some((a, b))) => a > b,
            (Op::Le, Some((a, b))) => a <= b,
            (Op::Ge, Some((a, b))) => a >= b,
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list() -> Value {
        json!({"kind": "List", "items": [
            {"metadata": {"name": "a", "labels": {"app.kubernetes.io/name": "x"}},
             "spec": {"containers": [{"name": "app", "image": "app:1"}, {"name": "side", "image": "side:1"}], "replicas": 3}},
            {"metadata": {"name": "b"}, "spec": {"containers": [{"name": "app", "image": "app:2"}], "replicas": 1}}
        ]})
    }

    fn r(template: &str) -> String {
        render(template, &list()).unwrap()
    }

    #[test]
    fn paths_pick_values() {
        assert_eq!(r("{.kind}"), "List");
        assert_eq!(r("{.items[*].metadata.name}"), "a b");
        assert_eq!(r("{.items[0].spec.containers[1].image}"), "side:1");
        assert_eq!(r("{.items[-1].metadata.name}"), "b");
        assert_eq!(r("{.items[0:1].metadata.name}"), "a");
        assert_eq!(r("{.items[*].spec.containers[*].image}"), "app:1 side:1 app:2");
        assert_eq!(r("{.items[0].metadata.labels['app.kubernetes.io/name']}"), "x");
        assert_eq!(r("{.items[0].metadata.labels.app\\.x}"), "");
        assert_eq!(r("{items[0].metadata.name}"), "a");
        assert_eq!(r("{$.items[1].metadata.name}"), "b");
        assert_eq!(r("{.items[0].spec.replicas}"), "3");
        assert!(r("{.items[0].metadata}").contains("\"name\":\"a\""));
    }

    #[test]
    fn missing_values_print_nothing() {
        assert_eq!(r("{.nope}"), "");
        assert_eq!(r("[{.items[5].metadata.name}]"), "[]");
        assert!(r("{.items[*].metadata.labels}").starts_with("{\"app"));
    }

    #[test]
    fn text_literals_and_ranges() {
        assert_eq!(r("name={.items[0].metadata.name}!"), "name=a!");
        assert_eq!(
            r("{range .items[*]}{.metadata.name}{\"\\t\"}{.spec.replicas}{\"\\n\"}{end}"),
            "a\t3\nb\t1\n"
        );
        assert_eq!(
            r("{range .items[*]}{.metadata.name}:{range .spec.containers[*]}{.name},{end};{end}"),
            "a:app,side,;b:app,;"
        );
        assert_eq!(r("{'a b'}{\"\\\"q\\\"\"}"), "a b\"q\"");
    }

    #[test]
    fn filters() {
        assert_eq!(
            r("{.items[*].spec.containers[?(@.name==\"app\")].image}"),
            "app:1 app:2"
        );
        assert_eq!(r("{.items[?(@.spec.replicas>1)].metadata.name}"), "a");
        assert_eq!(r("{.items[?(@.spec.replicas!=3)].metadata.name}"), "b");
        assert_eq!(r("{.items[?(@.metadata.labels)].metadata.name}"), "a");
        assert_eq!(r("{.items[?(@.spec.replicas<=1)].metadata.name}"), "b");
    }

    #[test]
    fn errors_name_the_template() {
        let e = |t: &str| render(t, &list()).unwrap_err();
        assert_eq!(e("{.items"), "error parsing jsonpath {.items, unclosed action");
        assert_eq!(e("{end}"), "error parsing jsonpath {end}, unexpected {end}");
        assert_eq!(
            e("{range .items[*]}x"),
            "error parsing jsonpath {range .items[*]}x, range action missing end"
        );
        assert!(e("{..name}").contains("recursive descent (..) is not supported"));
        assert!(e("{.items[x]}").contains("invalid array index \"x\""));
        assert!(e("{.items[0}").contains("unterminated array subscript"));
        assert!(e("{.a..b}").contains("recursive descent"));
        assert!(e("{.items[?(@.x==nope)]}").contains("invalid filter value"));
    }
}
