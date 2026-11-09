//! `k8s.io/apimachinery/pkg/util/validation` and `.../api/validate/content`: the name, label,
//! key and port rules with their exact messages.

const DNS1123_LABEL_FMT: &str = "[a-z0-9]([-a-z0-9]*[a-z0-9])?";
const DNS1123_LABEL_ERR: &str = "a lowercase RFC 1123 label must consist of lower case alphanumeric characters or '-', and must start and end with an alphanumeric character";
const DNS1123_SUBDOMAIN_FMT: &str = "[a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*";
const DNS1123_SUBDOMAIN_ERR: &str = "a lowercase RFC 1123 subdomain must consist of lower case alphanumeric characters, '-' or '.', and must start and end with an alphanumeric character";
const LABEL_KEY_FMT: &str = "([A-Za-z0-9][-A-Za-z0-9_.]*)?[A-Za-z0-9]";
const LABEL_KEY_ERR: &str =
    "must consist of alphanumeric characters, '-', '_' or '.', and must start and end with an alphanumeric character";
const LABEL_VALUE_FMT: &str = "(([A-Za-z0-9][-A-Za-z0-9_.]*)?[A-Za-z0-9])?";
const LABEL_VALUE_ERR: &str = "a valid label must be an empty string or consist of alphanumeric characters, '-', '_' or '.', and must start and end with an alphanumeric character";
const CONFIG_MAP_KEY_FMT: &str = "[-._a-zA-Z0-9]+";
const CONFIG_MAP_KEY_ERR: &str = "a valid config key must consist of alphanumeric characters, '-', '_' or '.'";
const PERCENT_FMT: &str = "[0-9]+%";
const PERCENT_ERR: &str = "a valid percent string must be a numeric string followed by an ending '%'";
const HTTP_HEADER_FMT: &str = "[-A-Za-z0-9]+";
const HTTP_HEADER_ERR: &str = "a valid HTTP header must consist of alphanumeric characters or '-'";
const RELAXED_ENV_ERR: &str =
    "a valid environment variable name must consist only of printable ASCII characters other than '='";

pub const DNS1123_LABEL_MAX: usize = 63;
const DNS1123_SUBDOMAIN_MAX: usize = 253;

/// `validation.MaxLenError` ("characters").
pub fn max_len_error(n: usize) -> String {
    format!("must be no more than {n} characters")
}

/// `content.MaxLenError` ("bytes", for label values and label key names).
fn max_len_bytes_error(n: usize) -> String {
    format!("must be no more than {n} bytes")
}

pub fn regex_error(msg: &str, fmt: &str, examples: &[&str]) -> String {
    if examples.is_empty() {
        return format!("{msg} (regex used for validation is '{fmt}')");
    }
    let mut out = format!("{msg} (e.g. ");
    for (i, e) in examples.iter().enumerate() {
        if i > 0 {
            out.push_str(" or ");
        }
        out.push_str(&format!("'{e}', "));
    }
    out.push_str(&format!("regex used for validation is '{fmt}')"));
    out
}

pub fn inclusive_range_error(lo: i64, hi: i64) -> String {
    format!("must be between {lo} and {hi}, inclusive")
}

fn lower_alnum(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

fn is_dns1123_label_text(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && lower_alnum(b[0]) && lower_alnum(b[b.len() - 1]) && b.iter().all(|c| lower_alnum(*c) || *c == b'-')
}

fn is_dns1123_subdomain_text(s: &str) -> bool {
    s.split('.').all(is_dns1123_label_text)
}

pub fn is_dns1123_label(value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if value.len() > DNS1123_LABEL_MAX {
        errs.push(max_len_error(DNS1123_LABEL_MAX));
    }
    if !is_dns1123_label_text(value) {
        if is_dns1123_subdomain_text(value) {
            errs.push("must not contain dots".into());
        } else {
            errs.push(regex_error(
                DNS1123_LABEL_ERR,
                DNS1123_LABEL_FMT,
                &["my-name", "123-abc"],
            ));
        }
    }
    errs
}

pub fn is_dns1123_subdomain(value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if value.len() > DNS1123_SUBDOMAIN_MAX {
        errs.push(max_len_error(DNS1123_SUBDOMAIN_MAX));
    }
    if !is_dns1123_subdomain_text(value) {
        errs.push(regex_error(
            DNS1123_SUBDOMAIN_ERR,
            DNS1123_SUBDOMAIN_FMT,
            &["example.com"],
        ));
    }
    errs
}

fn label_key_text(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(c))
}

/// `IsQualifiedName` / `IsLabelKey`.
pub fn is_qualified_name(value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let parts: Vec<&str> = value.split('/').collect();
    let name = match parts.as_slice() {
        [name] => *name,
        [prefix, name] => {
            if prefix.is_empty() {
                errs.push("prefix part must be non-empty".into());
            } else {
                for msg in is_dns1123_subdomain(prefix) {
                    errs.push(format!("prefix part {msg}"));
                }
            }
            *name
        }
        _ => {
            errs.push(format!(
                "a valid label key {} with an optional DNS subdomain prefix and '/' (e.g. 'example.com/MyName')",
                regex_error(LABEL_KEY_ERR, LABEL_KEY_FMT, &["MyName", "my.name", "123-abc"])
            ));
            return errs;
        }
    };
    if name.is_empty() {
        errs.push("name part must be non-empty".into());
    } else if name.len() > DNS1123_LABEL_MAX {
        errs.push(format!("name part {}", max_len_bytes_error(DNS1123_LABEL_MAX)));
    }
    if !label_key_text(name) {
        errs.push(format!(
            "name part {}",
            regex_error(LABEL_KEY_ERR, LABEL_KEY_FMT, &["MyName", "my.name", "123-abc"])
        ));
    }
    errs
}

/// `IsValidLabelValue`.
pub fn is_valid_label_value(value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if value.len() > DNS1123_LABEL_MAX {
        errs.push(max_len_bytes_error(DNS1123_LABEL_MAX));
    }
    if !value.is_empty() && !label_key_text(value) {
        errs.push(regex_error(
            LABEL_VALUE_ERR,
            LABEL_VALUE_FMT,
            &["MyValue", "my_value", "12345"],
        ));
    }
    errs
}

fn has_chdir_prefix(value: &str) -> Vec<String> {
    match value {
        "." => vec!["must not be '.'".into()],
        ".." => vec!["must not be '..'".into()],
        v if v.starts_with("..") => vec!["must not start with '..'".into()],
        _ => Vec::new(),
    }
}

pub fn is_config_map_key(value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if value.len() > DNS1123_SUBDOMAIN_MAX {
        errs.push(max_len_error(DNS1123_SUBDOMAIN_MAX));
    }
    let ok = !value.is_empty() && value.bytes().all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c));
    if !ok {
        errs.push(regex_error(
            CONFIG_MAP_KEY_ERR,
            CONFIG_MAP_KEY_FMT,
            &["key.name", "KEY_NAME", "key-name"],
        ));
    }
    errs.extend(has_chdir_prefix(value));
    errs
}

/// `IsRelaxedEnvVarName` (the RelaxedEnvironmentVariableValidation rule, GA).
pub fn is_relaxed_env_var_name(value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if value.is_empty() {
        errs.push("environment variable name must be non-empty".into());
    }
    if value.chars().any(|c| !c.is_ascii() || c.is_ascii_control() || c == '=') {
        errs.push(RELAXED_ENV_ERR.into());
    }
    errs
}

pub fn is_valid_port_num(port: i64) -> Vec<String> {
    if (1..=65535).contains(&port) {
        Vec::new()
    } else {
        vec![inclusive_range_error(1, 65535)]
    }
}

pub fn is_valid_port_name(port: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if port.len() > 15 {
        errs.push(max_len_error(15));
    }
    if port.is_empty()
        || !port
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        errs.push("must contain only alpha-numeric characters (a-z, 0-9), and hyphens (-)".into());
    }
    if !port.bytes().any(|c| c.is_ascii_lowercase()) {
        errs.push("must contain at least one letter (a-z)".into());
    }
    if port.contains("--") {
        errs.push("must not contain consecutive hyphens".into());
    }
    if port.starts_with('-') || port.ends_with('-') {
        errs.push("must not begin or end with a hyphen".into());
    }
    errs
}

pub fn is_valid_percent(value: &str) -> Vec<String> {
    let ok = value.len() > 1 && value.ends_with('%') && value[..value.len() - 1].bytes().all(|c| c.is_ascii_digit());
    if ok {
        Vec::new()
    } else {
        vec![regex_error(PERCENT_ERR, PERCENT_FMT, &["1%", "93%"])]
    }
}

pub fn is_http_header_name(value: &str) -> Vec<String> {
    let ok = !value.is_empty() && value.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    if ok {
        Vec::new()
    } else {
        vec![regex_error(HTTP_HEADER_ERR, HTTP_HEADER_FMT, &["X-Header-Name"])]
    }
}

pub fn is_valid_user_id(uid: i64) -> Vec<String> {
    if (0..=i32::MAX as i64).contains(&uid) {
        Vec::new()
    } else {
        vec![inclusive_range_error(0, i32::MAX as i64)]
    }
}

/// `ValidateNameFunc`s: `NameIsDNSSubdomain`, `NameIsDNSLabel`.
#[derive(Clone, Copy)]
pub enum NameRule {
    Subdomain,
    Label,
}

impl NameRule {
    /// The rule's messages for a name (`prefix`: a generateName, which may end in `-`).
    pub fn check(self, name: &str, prefix: bool) -> Vec<String> {
        let name = if prefix {
            mask_trailing_dash(name)
        } else {
            name.to_string()
        };
        match self {
            NameRule::Subdomain => is_dns1123_subdomain(&name),
            NameRule::Label => is_dns1123_label(&name),
        }
    }
}

fn mask_trailing_dash(name: &str) -> String {
    if name.len() > 1 && name.ends_with('-') {
        format!("{}a", &name[..name.len() - 1])
    } else {
        name.to_string()
    }
}
