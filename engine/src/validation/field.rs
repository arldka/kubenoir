//! `k8s.io/apimachinery/pkg/util/validation/field`: field paths and errors, and the text an
//! error has in a Status cause (`ErrorBody`), which is what kubectl prints after the path.

use serde_json::Value;

/// A field path (`spec.containers[0].ports[1].name`, `data[key]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path(String);

impl Path {
    pub fn new(root: &str) -> Path {
        Path(root.to_string())
    }

    pub fn child(&self, name: &str) -> Path {
        if self.0.is_empty() {
            Path(name.to_string())
        } else {
            Path(format!("{}.{name}", self.0))
        }
    }

    pub fn index(&self, i: usize) -> Path {
        Path(format!("{}[{i}]", self.0))
    }

    pub fn key(&self, k: &str) -> Path {
        Path(format!("{}[{k}]", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `field.ErrorType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorType {
    NotFound,
    Required,
    Duplicate,
    Invalid,
    NotSupported,
    Forbidden,
    TooLong,
}

impl ErrorType {
    fn text(self) -> &'static str {
        match self {
            ErrorType::NotFound => "Not found",
            ErrorType::Required => "Required value",
            ErrorType::Duplicate => "Duplicate value",
            ErrorType::Invalid => "Invalid value",
            ErrorType::NotSupported => "Unsupported value",
            ErrorType::Forbidden => "Forbidden",
            ErrorType::TooLong => "Too long",
        }
    }
}

/// A bad value as `ErrorBody` prints it.
#[derive(Clone, Debug)]
pub enum Bad {
    /// Not printed (`omitValue`).
    Omit,
    /// int64, int32, float, bool: `%v`.
    Plain(String),
    /// A Go string: `%q`.
    Str(String),
    /// Anything else: its JSON (already rendered the way Go marshals it).
    Json(String),
}

impl Bad {
    pub fn int(n: i64) -> Bad {
        Bad::Plain(n.to_string())
    }

    pub fn str(s: &str) -> Bad {
        Bad::Str(s.to_string())
    }

    /// A JSON value with Go's map encoding (sorted keys, HTML escaped).
    pub fn json(v: &Value) -> Bad {
        Bad::Json(go_json(v))
    }
}

#[derive(Clone, Debug)]
pub struct FieldError {
    pub kind: ErrorType,
    pub field: Path,
    pub bad: Bad,
    pub detail: String,
}

impl FieldError {
    /// `ErrorBody()`.
    pub fn body(&self) -> String {
        let mut s = match self.kind {
            ErrorType::Required | ErrorType::Forbidden | ErrorType::TooLong => self.kind.text().to_string(),
            _ => match &self.bad {
                Bad::Omit => self.kind.text().to_string(),
                Bad::Plain(v) | Bad::Json(v) => format!("{}: {v}", self.kind.text()),
                Bad::Str(v) => format!("{}: {}", self.kind.text(), go_quote(v)),
            },
        };
        if !self.detail.is_empty() {
            s.push_str(": ");
            s.push_str(&self.detail);
        }
        s
    }

    /// What kubectl prints for the cause: `<field>: <body>`.
    pub fn cause(&self) -> String {
        format!("{}: {}", self.field.as_str(), self.body())
    }
}

pub type ErrorList = Vec<FieldError>;

fn err(kind: ErrorType, field: &Path, bad: Bad, detail: &str) -> FieldError {
    FieldError {
        kind,
        field: field.clone(),
        bad,
        detail: detail.to_string(),
    }
}

pub fn required(field: &Path, detail: &str) -> FieldError {
    err(ErrorType::Required, field, Bad::Omit, detail)
}

pub fn forbidden(field: &Path, detail: &str) -> FieldError {
    err(ErrorType::Forbidden, field, Bad::Omit, detail)
}

pub fn invalid(field: &Path, bad: Bad, detail: &str) -> FieldError {
    err(ErrorType::Invalid, field, bad, detail)
}

pub fn not_found(field: &Path, bad: Bad) -> FieldError {
    err(ErrorType::NotFound, field, bad, "")
}

pub fn duplicate(field: &Path, bad: Bad) -> FieldError {
    err(ErrorType::Duplicate, field, bad, "")
}

pub fn too_long(field: &Path, max: usize) -> FieldError {
    err(
        ErrorType::TooLong,
        field,
        Bad::Omit,
        &format!("may not be more than {max} bytes"),
    )
}

/// `field.NotSupported(path, value, validValues)`: the valid values quoted, in the order given.
pub fn not_supported(field: &Path, bad: Bad, valid: &[&str]) -> FieldError {
    let detail = if valid.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = valid.iter().map(|v| go_quote(v)).collect();
        format!("supported values: {}", quoted.join(", "))
    };
    err(ErrorType::NotSupported, field, bad, &detail)
}

/// Go's `strconv.Quote`.
pub fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What Go's `json.Marshal` writes for decoded JSON: sorted keys, `<`, `>`, `&` escaped.
pub fn go_json(v: &Value) -> String {
    let text = serde_json::to_string(v).expect("JSON values serialize");
    text.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}
