//! Objects written the way the API server stores them: decoded into upstream's Go types and
//! encoded again with encoding/json (SPEC V36). That round trip is visible in every `-o yaml`:
//! a struct field that is not a pointer is always written (`resources: {}`, a template's
//! `metadata: {}`), a field without `omitempty` is written even when zero (`endpoints: null`),
//! empty lists and maps tagged `omitempty` disappear, unknown fields are dropped, and types
//! that encode themselves do so (`divisor: "0"`, `cpu: "0.5"` → `500m`).
//!
//! The Go types come from [`crate::gotypes`], generated from upstream by `kubeoracle gotypes`;
//! strategic merge patch (`crate::smp`) walks the same graph.

use serde_json::{json, Map, Value};

use crate::gotypes::{NODES, ROOTS};
use crate::quantity::Quantity;

/// A Go type, as upstream's reflection sees it.
pub enum Node {
    Struct {
        name: &'static str,
        fields: &'static [Field],
    },
    Slice(u16),
    Map(u16),
    Ptr(u16),
    /// Any other Go kind (`string`, `int32`, `bool`, ...), by its `reflect.Kind` name.
    Scalar(&'static str),
    /// A type that encodes itself (`MarshalJSON`).
    Marshal {
        go_kind: &'static str,
        name: &'static str,
        marshal: Marshal,
    },
}

/// How a self-encoding type writes itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Marshal {
    /// `resource.Quantity`: its canonical text; zero is `"0"`.
    Quantity,
    /// `metav1.Time` / `MicroTime`: RFC 3339 text; zero is `null`.
    Time,
    /// `intstr.IntOrString`: the number or the text; zero is `0`.
    IntOrString,
    /// Raw JSON kept as given (`RawExtension`, `FieldsV1`); zero is `null`.
    Raw,
    /// `[]byte`: base64 text; nil is `null`.
    Bytes,
}

/// A struct field: its JSON name, strategic merge patch tags and encoding options.
pub struct Field {
    pub json: &'static str,
    pub strategies: &'static str,
    pub merge_key: &'static str,
    pub omit_empty: bool,
    pub omit_zero: bool,
    pub node: u16,
}

pub fn node(i: u16) -> &'static Node {
    &NODES[i as usize]
}

/// The node of a kind's Go type.
pub fn root(api_version: &str, kind: &str) -> Option<u16> {
    ROOTS
        .iter()
        .find(|(v, k, _)| *v == api_version && *k == kind)
        .map(|(_, _, n)| *n)
}

/// `object` as the API server would store it, if its kind is one of the simulated ones;
/// anything else is returned unchanged.
pub fn encode_object(object: &Value) -> Value {
    let api_version = object.get("apiVersion").and_then(Value::as_str).unwrap_or_default();
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or_default();
    match root(api_version, kind) {
        Some(n) => encode(n, Some(object)),
        None => object.clone(),
    }
}

/// JSON `null` decodes to Go's zero value: the same as an absent field.
fn present(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}

/// What encoding/json writes for a value of Go type `n` decoded from `v` (`None`: zero).
fn encode(n: u16, v: Option<&Value>) -> Value {
    let v = present(v);
    match node(n) {
        Node::Struct { fields, .. } => {
            let input = v.and_then(Value::as_object);
            let mut out = Map::new();
            for f in fields.iter() {
                let fv = present(input.and_then(|o| o.get(f.json)));
                if f.omit_empty && is_empty(f.node, fv) {
                    continue;
                }
                if f.omit_zero && is_zero(f.node, fv) {
                    continue;
                }
                out.insert(f.json.to_string(), encode(f.node, fv));
            }
            Value::Object(out)
        }
        Node::Ptr(elem) => match v {
            None => Value::Null,
            Some(v) => encode(*elem, Some(v)),
        },
        Node::Slice(elem) => match v {
            None => Value::Null,
            Some(Value::Array(items)) => Value::Array(items.iter().map(|x| encode(*elem, Some(x))).collect()),
            Some(other) => other.clone(),
        },
        Node::Map(elem) => match v {
            None => Value::Null,
            Some(Value::Object(m)) => {
                Value::Object(m.iter().map(|(k, x)| (k.clone(), encode(*elem, Some(x)))).collect())
            }
            Some(other) => other.clone(),
        },
        Node::Scalar(kind) => match v {
            Some(v) => v.clone(),
            None => scalar_zero(kind),
        },
        Node::Marshal { marshal, .. } => match (marshal, v) {
            (Marshal::Quantity, None) => json!("0"),
            (Marshal::Quantity, Some(v)) => {
                let text = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    other => return other.clone(),
                };
                Quantity::parse(&text).map_or_else(|| v.clone(), |q| json!(q.canonical()))
            }
            (Marshal::IntOrString, None) => json!(0),
            (Marshal::Time | Marshal::Raw | Marshal::Bytes, None) => Value::Null,
            (_, Some(v)) => v.clone(),
        },
    }
}

fn scalar_zero(kind: &str) -> Value {
    match kind {
        "string" => json!(""),
        "bool" => json!(false),
        _ => json!(0),
    }
}

fn scalar_is_zero(v: &Value) -> bool {
    match v {
        Value::String(s) => s.is_empty(),
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Null => true,
        _ => false,
    }
}

/// encoding/json's `isEmptyValue` (what `omitempty` omits): false, 0, "", nil pointers, and
/// empty slices and maps; never a struct.
fn is_empty(n: u16, v: Option<&Value>) -> bool {
    match node(n) {
        Node::Struct { .. } => false,
        Node::Marshal { go_kind, .. } => match (*go_kind, v) {
            ("struct", _) => false,
            (_, None) => true,
            (_, Some(Value::String(s))) => s.is_empty(),
            (_, Some(Value::Array(a))) => a.is_empty(),
            _ => false,
        },
        Node::Ptr(_) => v.is_none(),
        Node::Slice(_) => v.is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
        Node::Map(_) => v.is_none_or(|v| v.as_object().is_some_and(Map::is_empty)),
        Node::Scalar(_) => v.is_none_or(scalar_is_zero),
    }
}

/// What `omitzero` omits: the type's own `IsZero` where it has one (Quantity, Time), else
/// Go's zero value (a nil slice or map, but not an empty one).
fn is_zero(n: u16, v: Option<&Value>) -> bool {
    let Some(v) = v else { return true };
    match node(n) {
        Node::Struct { fields, .. } => {
            let o = v.as_object();
            fields
                .iter()
                .all(|f| is_zero(f.node, present(o.and_then(|o| o.get(f.json)))))
        }
        Node::Ptr(_) | Node::Slice(_) | Node::Map(_) => false,
        Node::Scalar(_) => scalar_is_zero(v),
        Node::Marshal { marshal, .. } => match marshal {
            Marshal::Quantity => {
                let text = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => return false,
                };
                Quantity::parse(&text).is_some_and(|q| q.canonical() == "0")
            }
            Marshal::Time => v.as_str() == Some("0001-01-01T00:00:00Z"),
            Marshal::IntOrString => v.as_f64() == Some(0.0),
            Marshal::Raw | Marshal::Bytes => false,
        },
    }
}
