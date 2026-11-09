//! Strategic merge patch, ported from upstream `k8s.io/apimachinery/pkg/util/strategicpatch`
//! (v0.36, `patch.go`): the merge half (`StrategicMergePatch` → `mergeMap` and friends) with the
//! same directives, ordering rules and error texts. Functions keep upstream's names (snake_case)
//! and order of operations so the two can be read side by side; `tests/fidelity.rs` checks the
//! port against upstream itself (SPEC V33, V34).
//!
//! Where upstream reads `patchStrategy` / `patchMergeKey` struct tags by reflection, the port
//! reads the same tags from [`crate::patchmeta`], generated from upstream's Go types.
//!
//! Go semantics kept on purpose: JSON numbers are float64 (`1` equals `1.0`), interface equality
//! (`==`) for merge keys and scalars, `%v` formatting of values in error texts, and
//! `sort.SliceStable`'s exact algorithm (upstream's comparator is not a strict order, so a
//! different stable sort could order lists differently).

use serde_json::{Map, Value};

use crate::patchmeta::{NODES, ROOTS};

type JsonMap = Map<String, Value>;

const DIRECTIVE_MARKER: &str = "$patch";
const DELETE_DIRECTIVE: &str = "delete";
const REPLACE_DIRECTIVE: &str = "replace";
const MERGE_DIRECTIVE: &str = "merge";
const RETAIN_KEYS_STRATEGY: &str = "retainKeys";
const DELETE_FROM_PRIMITIVE_LIST_PREFIX: &str = "$deleteFromPrimitiveList";
const RETAIN_KEYS_DIRECTIVE: &str = "$retainKeys";
const SET_ELEMENT_ORDER_PREFIX: &str = "$setElementOrder";

const ERR_BAD_JSON_DOC: &str = "invalid JSON document";
const ERR_NO_LIST_OF_LISTS: &str = "lists of lists are not supported";
const ERR_BAD_PRIMITIVE_LIST: &str = "invalid patch format of primitive list";
const ERR_BAD_RETAIN_KEYS: &str = "invalid patch format of retainKeys";
const ERR_BAD_SET_ELEMENT_ORDER: &str = "invalid patch format of setElementOrder list";
const ERR_UNSUPPORTED: &str = "strategic merge patch format is not supported";

/// Errors the API server reports as `400 BadRequest` (`interpretStrategicMergePatchError`);
/// every other patch error reaches kubectl as a reason-less server error.
pub fn is_bad_request(message: &str) -> bool {
    [
        ERR_BAD_JSON_DOC,
        ERR_BAD_PRIMITIVE_LIST,
        ERR_BAD_RETAIN_KEYS,
        ERR_BAD_SET_ELEMENT_ORDER,
        ERR_UNSUPPORTED,
    ]
    .contains(&message)
}

// ---- the type graph (`patchmeta.rs`) ----

/// A Go type strategic merge patch can meet, as upstream's reflection sees it.
pub enum Node {
    Struct {
        name: &'static str,
        fields: &'static [Field],
    },
    Slice(u16),
    /// Upstream never looks into a map's element type: merging into one fails if it would.
    Map,
    Ptr(u16),
    /// Any other Go kind (`string`, `int32`, `bool`, ...), by its `reflect.Kind` name.
    Scalar(&'static str),
}

/// A struct field: its JSON name and its `patchStrategy` / `patchMergeKey` tags.
pub struct Field {
    pub json: &'static str,
    pub strategies: &'static str,
    pub merge_key: &'static str,
    pub node: u16,
}

/// `PatchMetaFromStruct`: a position in the type graph.
#[derive(Clone, Copy)]
pub(crate) struct Schema(u16);

struct PatchMeta {
    strategies: Vec<&'static str>,
    merge_key: &'static str,
}

impl Schema {
    fn node(self) -> &'static Node {
        &NODES[self.0 as usize]
    }

    /// `reflect.Kind.String()` of the type.
    fn name(self) -> &'static str {
        match self.node() {
            Node::Struct { .. } => "struct",
            Node::Slice(_) => "slice",
            Node::Map => "map",
            Node::Ptr(_) => "ptr",
            Node::Scalar(kind) => kind,
        }
    }

    /// `forkedjson.LookupPatchMetadataForStruct`.
    fn lookup_patch_metadata_for_struct(self, key: &str) -> Result<(Schema, PatchMeta), String> {
        let mut t = self;
        if let Node::Ptr(elem) = t.node() {
            t = Schema(*elem);
        }
        let Node::Struct { name, fields } = t.node() else {
            return Err(format!(
                "merging an object in json but data type is not struct, instead is: {}",
                t.name()
            ));
        };
        // An exact match wins; otherwise the first case-insensitive one.
        let field = fields
            .iter()
            .find(|f| f.json == key)
            .or_else(|| fields.iter().find(|f| f.json.eq_ignore_ascii_case(key)));
        match field {
            Some(f) => Ok((
                Schema(f.node),
                PatchMeta {
                    strategies: f.strategies.split(',').collect(),
                    merge_key: f.merge_key,
                },
            )),
            None => Err(format!(
                "unable to find api field in struct {name} for the json field {}",
                go_quote(key)
            )),
        }
    }

    /// `PatchMetaFromStruct.LookupPatchMetadataForSlice`.
    fn lookup_patch_metadata_for_slice(self, key: &str) -> Result<(Schema, PatchMeta), String> {
        let (sub, meta) = self.lookup_patch_metadata_for_struct(key)?;
        let elem = match sub.node() {
            Node::Slice(elem) => {
                let elem = Schema(*elem);
                if matches!(elem.node(), Node::Slice(_)) {
                    return Err("unexpected slice of slice".into());
                }
                elem
            }
            Node::Ptr(elem) => match NODES[*elem as usize] {
                Node::Slice(inner) => Schema(inner),
                _ => Schema(*elem),
            },
            _ => return Err(format!("expected slice or array type, but got: {}", self.name())),
        };
        Ok((elem, meta))
    }
}

fn root(api_version: &str, kind: &str) -> Option<Schema> {
    ROOTS
        .iter()
        .find(|(v, k, _)| *v == api_version && *k == kind)
        .map(|(_, _, node)| Schema(*node))
}

#[derive(Clone, Copy)]
pub struct MergeOptions {
    pub merge_parallel_list: bool,
    pub ignore_unmatched_nulls: bool,
}

/// `StrategicMergePatch`: `patch` applied to `original`, an object of `apiVersion`/`kind`, the
/// way the API server applies `kubectl patch --type strategic` (and client-side `apply`).
pub fn strategic_merge_patch(original: &Value, patch: &Value) -> Result<Value, String> {
    let (Value::Object(original), Value::Object(patch)) = (original, patch) else {
        return Err(ERR_BAD_JSON_DOC.into());
    };
    let api_version = original.get("apiVersion").and_then(Value::as_str).unwrap_or_default();
    let kind = original.get("kind").and_then(Value::as_str).unwrap_or_default();
    let schema = root(api_version, kind).ok_or(ERR_UNSUPPORTED)?;
    let options = MergeOptions {
        merge_parallel_list: true,
        ignore_unmatched_nulls: true,
    };
    merge_map(original.clone(), patch.clone(), schema, options).map(Value::Object)
}

// ---- Go value semantics ----

/// Go's `==` on decoded JSON (`interface{}` holding float64, string, bool or nil). Maps and
/// slices are not comparable in Go (it would panic); they compare deeply here.
fn go_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// Same Go dynamic type (`reflect.TypeOf`).
fn same_type(a: &Value, b: &Value) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// `reflect.TypeOf(v).String()` for decoded JSON.
fn go_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "<nil>",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float64",
        Value::String(_) => "string",
        Value::Array(_) => "[]interface {}",
        Value::Object(_) => "map[string]interface {}",
    }
}

const GO_MAP: &str = "map[string]interface {}";
const GO_SLICE: &str = "[]interface {}";

/// Go's `%v` of a float64 (`strconv` 'g', shortest digits): `%e` form when the decimal exponent
/// is < -4 or >= 6, so `999999` and `0.0001` print plainly, `1e+06` and `1e-05` do not.
fn go_float(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{f:e}"); // shortest round-trip digits, e.g. "1.23456789e8"
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        format!("{f}")
    }
}

/// Go's `%v` of decoded JSON: maps with sorted keys as `map[k:v ...]`, slices as `[a b]`.
fn go_v(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => go_float(n.as_f64().unwrap_or_default()),
        Value::String(s) => s.clone(),
        Value::Array(items) => go_v_list(items),
        Value::Object(map) => go_v_map(map),
    }
}

fn go_v_list(items: &[Value]) -> String {
    format!("[{}]", items.iter().map(go_v).collect::<Vec<_>>().join(" "))
}

fn go_v_map(map: &JsonMap) -> String {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let parts: Vec<String> = keys.iter().map(|k| format!("{k}:{}", go_v(&map[*k]))).collect();
    format!("map[{}]", parts.join(" "))
}

/// Go's `%s` of an `interface{}`: strings as is, anything else as `%!s(type=value)`.
fn go_s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "%!s(<nil>)".into(),
        other => format!("%!s({}={})", go_type(other), go_v(other)),
    }
}

/// Go's `%q` for the field names upstream quotes.
fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn err_bad_arg_type(expected: &str, actual: &Value) -> String {
    format!("expected a {expected}, but received a {}", go_type(actual))
}

fn err_no_merge_key(m: &JsonMap, key: &str) -> String {
    format!("map: {} does not contain declared merge key: {key}", go_v_map(m))
}

fn err_bad_patch_type(t: &Value, m: &JsonMap) -> String {
    format!("unknown patch type: {} in map: {}", go_s(t), go_v_map(m))
}

// ---- the port ----

/// `handleDirectiveInMergeMap`.
fn handle_directive_in_merge_map(directive: &Value, mut patch: JsonMap) -> Result<JsonMap, String> {
    if directive.as_str() == Some(REPLACE_DIRECTIVE) {
        patch.remove(DIRECTIVE_MARKER);
        return Ok(patch);
    }
    if directive.as_str() == Some(DELETE_DIRECTIVE) {
        return Ok(JsonMap::new());
    }
    Err(err_bad_patch_type(directive, &patch))
}

fn contains_directive_marker(item: &Value) -> bool {
    item.as_object().is_some_and(|m| m.contains_key(DIRECTIVE_MARKER))
}

/// `mergeKeyValueEqual`.
fn merge_key_value_equal(left: &Value, right: &Value, merge_key: &str) -> Result<bool, String> {
    if merge_key.is_empty() {
        return Ok(go_eq(left, right));
    }
    let typed_left = left.as_object().ok_or_else(|| err_bad_arg_type(GO_MAP, left))?;
    let typed_right = right.as_object().ok_or_else(|| err_bad_arg_type(GO_MAP, right))?;
    let l = typed_left
        .get(merge_key)
        .ok_or_else(|| err_no_merge_key(typed_left, merge_key))?;
    let r = typed_right
        .get(merge_key)
        .ok_or_else(|| err_no_merge_key(typed_right, merge_key))?;
    Ok(go_eq(l, r))
}

/// `extractKey`.
fn extract_key(s: &str, prefix: &str) -> Result<String, String> {
    match s.split_once('/') {
        Some((head, rest)) if head == prefix => Ok(rest.to_string()),
        _ => Err(match prefix {
            DELETE_FROM_PRIMITIVE_LIST_PREFIX => ERR_BAD_PRIMITIVE_LIST.into(),
            SET_ELEMENT_ORDER_PREFIX => ERR_BAD_SET_ELEMENT_ORDER.into(),
            _ => format!("fail to find unknown prefix {} in {s}\n", go_quote(prefix)),
        }),
    }
}

/// `validatePatchWithSetOrderList`.
fn validate_patch_with_set_order_list(
    patch_list: &[Value],
    set_order_list: &[Value],
    merge_key: &str,
) -> Result<(), String> {
    if set_order_list.is_empty() || patch_list.is_empty() {
        return Ok(());
    }
    let non_delete_list: Vec<Value> = if merge_key.is_empty() {
        patch_list.to_vec()
    } else {
        extract_to_delete_items(patch_list.to_vec())?.0
    };
    let (mut patch_index, mut set_order_index) = (0, 0);
    while patch_index < non_delete_list.len() && set_order_index < set_order_list.len() {
        if contains_directive_marker(&non_delete_list[patch_index]) {
            patch_index += 1;
            continue;
        }
        if merge_key_value_equal(
            &non_delete_list[patch_index],
            &set_order_list[set_order_index],
            merge_key,
        )? {
            patch_index += 1;
        }
        set_order_index += 1;
    }
    if patch_index < non_delete_list.len() && set_order_index >= set_order_list.len() {
        return Err(format!(
            "The order in patch list:\n{}\n doesn't match {SET_ELEMENT_ORDER_PREFIX} list:\n{}\n",
            go_v_list(patch_list),
            go_v_list(set_order_list)
        ));
    }
    Ok(())
}

/// `preprocessDeletionListForMerging`: (skip, is delete list, key without prefix).
fn preprocess_deletion_list_for_merging(
    key: &str,
    original: &mut JsonMap,
    patch_val: &Value,
    merge_deletion_list: bool,
) -> Result<(bool, bool, String), String> {
    if key.starts_with(DELETE_FROM_PRIMITIVE_LIST_PREFIX) {
        if !merge_deletion_list {
            original.insert(key.to_string(), patch_val.clone());
            return Ok((true, false, String::new()));
        }
        let original_key = extract_key(key, DELETE_FROM_PRIMITIVE_LIST_PREFIX)?;
        return Ok((false, true, original_key));
    }
    Ok((false, false, String::new()))
}

/// `applyRetainKeysDirective`.
fn apply_retain_keys_directive(
    original: &mut JsonMap,
    patch: &mut JsonMap,
    options: MergeOptions,
) -> Result<(), String> {
    let Some(retain_keys_in_patch) = patch.remove(RETAIN_KEYS_DIRECTIVE) else {
        return Ok(());
    };
    if !options.merge_parallel_list {
        match original.get(RETAIN_KEYS_DIRECTIVE) {
            Some(in_original) => {
                if in_original != &retain_keys_in_patch {
                    return Err(format!(
                        "{} and {} are not deep equal: this may happen when calculating the 3-way diff patch",
                        go_v(in_original),
                        go_v(&retain_keys_in_patch)
                    ));
                }
            }
            None => {
                original.insert(RETAIN_KEYS_DIRECTIVE.into(), retain_keys_in_patch);
            }
        }
        return Ok(());
    }
    let Value::Array(retain_keys_list) = retain_keys_in_patch else {
        return Err(ERR_BAD_RETAIN_KEYS.into());
    };
    let retained = |k: &str| retain_keys_list.iter().any(|v| v.as_str() == Some(k));
    for (k, v) in patch.iter() {
        if v.is_null() || k.starts_with(DELETE_FROM_PRIMITIVE_LIST_PREFIX) || k.starts_with(SET_ELEMENT_ORDER_PREFIX) {
            continue;
        }
        if !retained(k) {
            return Err(ERR_BAD_RETAIN_KEYS.into());
        }
    }
    original.retain(|k, _| retained(k));
    Ok(())
}

/// `mergePatchIntoOriginal`: `$setElementOrder` lists and the lists they order.
fn merge_patch_into_original(
    original: &mut JsonMap,
    patch: &mut JsonMap,
    schema: Schema,
    options: MergeOptions,
) -> Result<(), String> {
    let keys: Vec<String> = patch
        .keys()
        .filter(|k| k.starts_with(SET_ELEMENT_ORDER_PREFIX))
        .cloned()
        .collect();
    for key in keys {
        let Some(set_element_order_in_patch) = patch.get(&key).cloned() else {
            continue;
        };
        if !options.merge_parallel_list {
            match original.get(&key) {
                Some(in_original) if in_original != &set_element_order_in_patch => {
                    return Err(ERR_BAD_SET_ELEMENT_ORDER.into());
                }
                Some(_) => {}
                None => {
                    original.insert(key.clone(), set_element_order_in_patch.clone());
                }
            }
        }
        patch.remove(&key);

        let Value::Array(typed_set_element_order_list) = &set_element_order_in_patch else {
            return Err(err_bad_arg_type(GO_SLICE, &set_element_order_in_patch));
        };
        let original_key = extract_key(&key, SET_ELEMENT_ORDER_PREFIX)?;
        let original_list = original.get(&original_key).cloned();
        let patch_list = patch.get(&original_key).cloned();
        let original_field_value: Vec<Value> = match &original_list {
            Some(Value::Array(items)) => items.clone(),
            Some(other) => return Err(err_bad_arg_type(GO_SLICE, other)),
            None => Vec::new(),
        };
        let patch_field_value: Vec<Value> = match &patch_list {
            Some(Value::Array(items)) => items.clone(),
            Some(other) => return Err(err_bad_arg_type(GO_SLICE, other)),
            None => Vec::new(),
        };
        let (subschema, patch_meta) = schema.lookup_patch_metadata_for_slice(&original_key)?;
        let (_, patch_strategy) = extract_retain_keys_patch_strategy(&patch_meta.strategies)?;
        validate_patch_with_set_order_list(&patch_field_value, typed_set_element_order_list, patch_meta.merge_key)?;

        let merged: Vec<Value> = match (original_list.is_some(), patch_list.is_some()) {
            (true, false) => original_field_value.clone(),
            (false, true) => match remove_directives(Value::Array(patch_field_value.clone())) {
                Some(Value::Array(items)) => items,
                _ => continue,
            },
            (true, true) => merge_slice_handler(
                original_field_value.clone(),
                patch_field_value.clone(),
                subschema,
                patch_strategy,
                patch_meta.merge_key,
                false,
                options,
            )?,
            (false, false) => continue,
        };

        let (patch_items, server_only_items) = if patch_meta.merge_key.is_empty() {
            partition_primitives_by_present_in_list(&merged, typed_set_element_order_list)
        } else {
            partition_maps_by_present_in_list(&merged, typed_set_element_order_list, patch_meta.merge_key)?
        };
        let is_map = slice_element_type(&[&original_field_value, &patch_field_value])?;
        let both = normalize_element_order(
            patch_items,
            server_only_items,
            typed_set_element_order_list,
            &original_field_value,
            patch_meta.merge_key,
            is_map,
        )?;
        original.insert(original_key.clone(), Value::Array(both));
        patch.remove(&original_key);
    }
    Ok(())
}

/// `partitionPrimitivesByPresentInList`.
fn partition_primitives_by_present_in_list(original: &[Value], partition_by: &[Value]) -> (Vec<Value>, Vec<Value>) {
    original
        .iter()
        .cloned()
        .partition(|v| partition_by.iter().any(|p| go_eq(p, v)))
}

/// `partitionMapsByPresentInList`.
fn partition_maps_by_present_in_list(
    original: &[Value],
    partition_by: &[Value],
    merge_key: &str,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let (mut patch, mut server_only) = (Vec::new(), Vec::new());
    for v in original {
        let typed_v = v.as_object().ok_or_else(|| err_bad_arg_type(GO_MAP, v))?;
        let merge_key_value = typed_v
            .get(merge_key)
            .ok_or_else(|| err_no_merge_key(typed_v, merge_key))?;
        if find_map_in_slice_based_on_key_value(partition_by, merge_key, merge_key_value)?.is_some() {
            patch.push(v.clone());
        } else {
            server_only.push(v.clone());
        }
    }
    Ok((patch, server_only))
}

/// `removeDirectives`: the value without `$patch` maps, or `None` if it is one itself.
fn remove_directives(obj: Value) -> Option<Value> {
    match obj {
        Value::Object(mut map) => {
            if map.contains_key(DIRECTIVE_MARKER) {
                return None;
            }
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                let v = map.remove(&k).expect("key listed");
                if let Some(v) = remove_directives(v) {
                    map.insert(k, v);
                }
            }
            Some(Value::Object(map))
        }
        Value::Array(items) => Some(Value::Array(items.into_iter().filter_map(remove_directives).collect())),
        other => Some(other),
    }
}

/// `mergeMap`.
pub(crate) fn merge_map(
    mut original: JsonMap,
    mut patch: JsonMap,
    schema: Schema,
    options: MergeOptions,
) -> Result<JsonMap, String> {
    if let Some(v) = patch.get(DIRECTIVE_MARKER).cloned() {
        return handle_directive_in_merge_map(&v, patch);
    }
    apply_retain_keys_directive(&mut original, &mut patch, options)?;
    merge_patch_into_original(&mut original, &mut patch, schema, options)?;

    for (key, mut patch_v) in patch {
        let (skip, is_delete_list, no_prefix_key) =
            preprocess_deletion_list_for_merging(&key, &mut original, &patch_v, options.merge_parallel_list)?;
        if skip {
            continue;
        }
        let k = if no_prefix_key.is_empty() { key } else { no_prefix_key };

        if patch_v.is_null() {
            original.remove(&k);
            if options.ignore_unmatched_nulls {
                continue;
            }
        }

        let replace_with_patch = match original.get(&k) {
            None => true,
            Some(existing) => !same_type(existing, &patch_v),
        };
        if replace_with_patch {
            if !is_delete_list {
                if options.ignore_unmatched_nulls {
                    discard_null_values_from_patch(&mut patch_v);
                }
                match remove_directives(patch_v) {
                    Some(v) => {
                        original.insert(k, v);
                    }
                    None => {
                        original.remove(&k);
                    }
                }
            }
            continue;
        }

        let existing = original.remove(&k).expect("checked present");
        let merged = match (existing, patch_v) {
            (Value::Object(existing), Value::Object(patch_v)) => {
                let (subschema, patch_meta) = schema.lookup_patch_metadata_for_struct(&k)?;
                let (_, patch_strategy) = extract_retain_keys_patch_strategy(&patch_meta.strategies)?;
                Some(Value::Object(merge_map_handler(
                    existing,
                    patch_v,
                    subschema,
                    patch_strategy,
                    options,
                )?))
            }
            (Value::Array(existing), Value::Array(patch_v)) => {
                let (subschema, patch_meta) = schema.lookup_patch_metadata_for_slice(&k)?;
                let (_, patch_strategy) = extract_retain_keys_patch_strategy(&patch_meta.strategies)?;
                Some(Value::Array(merge_slice_handler(
                    existing,
                    patch_v,
                    subschema,
                    patch_strategy,
                    patch_meta.merge_key,
                    is_delete_list,
                    options,
                )?))
            }
            (_, patch_v) => remove_directives(patch_v),
        };
        if let Some(merged) = merged {
            original.insert(k, merged);
        }
    }
    Ok(original)
}

/// `discardNullValuesFromPatch`.
fn discard_null_values_from_patch(patch_v: &mut Value) {
    match patch_v {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(discard_null_values_from_patch);
        }
        Value::Array(items) => items.iter_mut().for_each(discard_null_values_from_patch),
        _ => {}
    }
}

/// `mergeMapHandler`.
fn merge_map_handler(
    original: JsonMap,
    patch: JsonMap,
    schema: Schema,
    field_patch_strategy: &str,
    options: MergeOptions,
) -> Result<JsonMap, String> {
    if field_patch_strategy != REPLACE_DIRECTIVE {
        merge_map(original, patch, schema, options)
    } else {
        Ok(patch)
    }
}

/// `mergeSliceHandler`.
fn merge_slice_handler(
    original: Vec<Value>,
    patch: Vec<Value>,
    schema: Schema,
    field_patch_strategy: &str,
    field_patch_merge_key: &str,
    is_delete_list: bool,
    options: MergeOptions,
) -> Result<Vec<Value>, String> {
    if field_patch_strategy == MERGE_DIRECTIVE || is_delete_list {
        merge_slice(original, patch, schema, field_patch_merge_key, options, is_delete_list)
    } else {
        Ok(patch)
    }
}

/// `mergeSlice`.
fn merge_slice(
    original: Vec<Value>,
    patch: Vec<Value>,
    schema: Schema,
    merge_key: &str,
    options: MergeOptions,
    is_delete_list: bool,
) -> Result<Vec<Value>, String> {
    if original.is_empty() && patch.is_empty() {
        return Ok(original);
    }
    let is_map = slice_element_type(&[&original, &patch])?;

    let (original, patch, merged) = if !is_map {
        if options.merge_parallel_list && is_delete_list {
            return Ok(delete_from_slice(&original, &patch));
        }
        let both: Vec<Value> = original.iter().chain(patch.iter()).cloned().collect();
        let merged = deduplicate_scalars(both);
        (original, patch, merged)
    } else {
        if merge_key.is_empty() {
            return Err(format!("cannot merge lists without merge key for {}", schema.name()));
        }
        let (original, patch) = merge_slice_with_special_elements(original, patch, merge_key)?;
        let merged = merge_slice_without_special_elements(original.clone(), &patch, merge_key, schema, options)?;
        (original, patch, merged)
    };

    let (patch_items, server_only_items) = if merge_key.is_empty() {
        partition_primitives_by_present_in_list(&merged, &patch)
    } else {
        partition_maps_by_present_in_list(&merged, &patch, merge_key)?
    };
    normalize_element_order(patch_items, server_only_items, &patch, &original, merge_key, is_map)
}

/// `mergeSliceWithSpecialElements`: deletions applied to `original`, `$patch` items dropped
/// from the patch; with `$patch: replace` the patch's items become the list.
fn merge_slice_with_special_elements(
    mut original: Vec<Value>,
    patch: Vec<Value>,
    merge_key: &str,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut patch_without_special_elements = Vec::new();
    let mut replace = false;
    for v in patch {
        let typed_v = v.as_object().expect("slice element type checked: maps");
        match typed_v.get(DIRECTIVE_MARKER) {
            None => patch_without_special_elements.push(v),
            Some(patch_type) => match patch_type.as_str() {
                Some(DELETE_DIRECTIVE) => match typed_v.get(merge_key) {
                    Some(merge_value) => {
                        original = delete_matching_entries(original, merge_key, merge_value)?;
                    }
                    None => return Err(err_no_merge_key(typed_v, merge_key)),
                },
                Some(REPLACE_DIRECTIVE) => replace = true,
                Some(MERGE_DIRECTIVE) => return Err("merging lists cannot yet be specified in the patch".into()),
                _ => return Err(err_bad_patch_type(patch_type, typed_v)),
            },
        }
    }
    if replace {
        return Ok((patch_without_special_elements, Vec::new()));
    }
    Ok((original, patch_without_special_elements))
}

/// `deleteMatchingEntries`.
fn delete_matching_entries(
    mut original: Vec<Value>,
    merge_key: &str,
    merge_value: &Value,
) -> Result<Vec<Value>, String> {
    while let Some(i) = find_map_in_slice_based_on_key_value(&original, merge_key, merge_value)? {
        original.remove(i);
    }
    Ok(original)
}

/// `mergeSliceWithoutSpecialElements`.
fn merge_slice_without_special_elements(
    mut original: Vec<Value>,
    patch: &[Value],
    merge_key: &str,
    schema: Schema,
    options: MergeOptions,
) -> Result<Vec<Value>, String> {
    for v in patch {
        let typed_v = v.as_object().expect("slice element type checked: maps");
        let merge_value = typed_v
            .get(merge_key)
            .ok_or_else(|| err_no_merge_key(typed_v, merge_key))?;
        match find_map_in_slice_based_on_key_value(&original, merge_key, merge_value)? {
            Some(i) => {
                let Value::Object(original_map) = std::mem::take(&mut original[i]) else {
                    unreachable!("found maps only")
                };
                original[i] = Value::Object(merge_map(original_map, typed_v.clone(), schema, options)?);
            }
            None => original.push(v.clone()),
        }
    }
    Ok(original)
}

/// `deleteFromSlice`.
fn delete_from_slice(current: &[Value], to_delete: &[Value]) -> Vec<Value> {
    current
        .iter()
        .filter(|v| !to_delete.iter().any(|d| go_eq(d, v)))
        .cloned()
        .collect()
}

/// `findMapInSliceBasedOnKeyValue`: the index of the first map whose `key` equals `value`.
fn find_map_in_slice_based_on_key_value(m: &[Value], key: &str, value: &Value) -> Result<Option<usize>, String> {
    for (k, v) in m.iter().enumerate() {
        let typed_v = v.as_object().ok_or_else(|| format!("value for key {k} is not a map"))?;
        if typed_v.get(key).is_some_and(|to_match| go_eq(to_match, value)) {
            return Ok(Some(k));
        }
    }
    Ok(None)
}

/// `deduplicateScalars`, including its order-scrambling swap-with-last.
fn deduplicate_scalars(mut s: Vec<Value>) -> Vec<Value> {
    if s.is_empty() {
        return s;
    }
    let mut length = s.len() - 1;
    let mut i = 0;
    while i < length {
        let mut j = i + 1;
        while j <= length {
            if go_eq(&s[i], &s[j]) {
                s[j] = s[length].clone();
                s.truncate(length);
                length -= 1;
                // `j--` then the loop's `j++`: look at the swapped-in value again.
                continue;
            }
            j += 1;
        }
        i += 1;
    }
    s
}

/// `sliceElementType`: whether the elements are maps; all elements must share one type.
fn slice_element_type(slices: &[&[Value]]) -> Result<bool, String> {
    let mut prev: Option<&Value> = None;
    for s in slices {
        for v in s.iter() {
            match prev {
                None => {
                    if v.is_array() {
                        return Err(ERR_NO_LIST_OF_LISTS.into());
                    }
                    prev = Some(v);
                }
                Some(p) => {
                    if !same_type(p, v) {
                        let lists: Vec<String> = slices.iter().map(|s| go_v_list(s)).collect();
                        return Err(format!("list element types are not identical: [{}]", lists.join(" ")));
                    }
                }
            }
        }
    }
    match prev {
        Some(v) => Ok(v.is_object()),
        None => Err("no elements in any of the given slices".into()),
    }
}

/// `extractRetainKeysPatchStrategy`: (has retainKeys, the other strategy).
fn extract_retain_keys_patch_strategy(strategies: &[&'static str]) -> Result<(bool, &'static str), String> {
    let unexpected = || format!("unexpected patch strategy: [{}]", strategies.join(" "));
    match strategies {
        [] => Ok((false, "")),
        [single] if *single == RETAIN_KEYS_STRATEGY => Ok((true, "")),
        [single] => Ok((false, single)),
        [first, second] if *first == RETAIN_KEYS_STRATEGY => Ok((true, second)),
        [first, second] if *second == RETAIN_KEYS_STRATEGY => Ok((true, first)),
        _ => Err(unexpected()),
    }
}

/// `extractToDeleteItems`: (items to keep, `$patch: delete` items).
fn extract_to_delete_items(l: Vec<Value>) -> Result<(Vec<Value>, Vec<Value>), String> {
    let (mut non_delete, mut to_delete) = (Vec::new(), Vec::new());
    for v in l {
        let m = v.as_object().ok_or_else(|| err_bad_arg_type(GO_MAP, &v))?;
        if m.get(DIRECTIVE_MARKER).and_then(Value::as_str) == Some(DELETE_DIRECTIVE) {
            to_delete.push(v);
        } else {
            non_delete.push(v);
        }
    }
    Ok((non_delete, to_delete))
}

/// `validateMergeKeyInLists`.
fn validate_merge_key_in_lists(merge_key: &str, lists: &[&[Value]]) -> Result<(), String> {
    for list in lists {
        for item in list.iter() {
            let m = item.as_object().ok_or_else(|| err_bad_arg_type(GO_MAP, item))?;
            if !m.contains_key(merge_key) {
                return Err(err_no_merge_key(m, merge_key));
            }
        }
    }
    Ok(())
}

/// The value `index` compares: the merge key's value for maps, the item itself otherwise.
/// `None` is Go's `nil` (a missing key, a JSON null or a non-map item).
fn order_value<'a>(item: &'a Value, merge_key: &str, is_map: bool) -> Option<&'a Value> {
    let v = if is_map {
        item.as_object().and_then(|m| m.get(merge_key))
    } else {
        Some(item)
    };
    v.filter(|v| !v.is_null())
}

/// `index`: the position of `val` in `l`, or -1.
fn index(l: &[Value], val: &Value, merge_key: &str, is_map: bool) -> isize {
    let wanted = order_value(val, merge_key, is_map);
    l.iter()
        .position(|v| match (wanted, order_value(v, merge_key, is_map)) {
            (None, None) => true,
            (Some(a), Some(b)) => go_eq(a, b),
            _ => false,
        })
        .map_or(-1, |i| i as isize)
}

/// `normalizeElementOrder`.
fn normalize_element_order(
    patch: Vec<Value>,
    server_only: Vec<Value>,
    patch_order: &[Value],
    server_order: &[Value],
    merge_key: &str,
    is_map: bool,
) -> Result<Vec<Value>, String> {
    let patch = normalize_slice_order(patch, patch_order, merge_key, is_map)?;
    let server_only = normalize_slice_order(server_only, server_order, merge_key, is_map)?;
    Ok(merge_sorted_slice(server_only, patch, server_order, merge_key, is_map))
}

/// `mergeSortedSlice`.
fn merge_sorted_slice(
    left: Vec<Value>,
    right: Vec<Value>,
    server_order: &[Value],
    merge_key: &str,
    is_map: bool,
) -> Vec<Value> {
    let less = |l: &Value, r: &Value| -> (bool, bool) {
        let li = index(server_order, l, merge_key, is_map);
        let ri = index(server_order, r, merge_key, is_map);
        if li >= 0 && ri >= 0 {
            (li < ri, true)
        } else {
            (false, false)
        }
    };
    let size = left.len() + right.len();
    let (mut i, mut j) = (0, 0);
    let mut s = Vec::with_capacity(size);
    for _ in 0..size {
        if i >= left.len() && j < right.len() {
            s.push(right[j].clone());
            j += 1;
        } else if j >= right.len() && i < left.len() {
            s.push(left[i].clone());
            i += 1;
        } else {
            let (less, found_both) = less(&left[i], &right[j]);
            if found_both && less {
                s.push(left[i].clone());
                i += 1;
            } else {
                s.push(right[j].clone());
                j += 1;
            }
        }
    }
    s
}

/// `normalizeSliceOrder`: `to_sort` in the order of `order`, `$patch: delete` items last.
fn normalize_slice_order(
    to_sort: Vec<Value>,
    order: &[Value],
    merge_key: &str,
    is_map: bool,
) -> Result<Vec<Value>, String> {
    let (mut to_sort, to_delete) = if is_map {
        validate_merge_key_in_lists(merge_key, &[&to_sort, order])?;
        extract_to_delete_items(to_sort)?
    } else {
        (to_sort, Vec::new())
    };
    go_slice_stable(&mut to_sort, |a, b| {
        let ii = index(order, a, merge_key, is_map);
        if ii >= 0 {
            let ij = index(order, b, merge_key, is_map);
            if ij >= 0 {
                return ii < ij;
            }
        }
        true
    });
    to_sort.extend(to_delete);
    Ok(to_sort)
}

// ---- Go's sort.SliceStable (sort/zsortfunc.go), step for step ----

fn go_slice_stable<T>(data: &mut [T], less: impl Fn(&T, &T) -> bool) {
    let n = data.len();
    let mut block_size = 20;
    let (mut a, mut b) = (0, block_size);
    while b <= n {
        insertion_sort(data, a, b, &less);
        a = b;
        b += block_size;
    }
    insertion_sort(data, a, n, &less);
    while block_size < n {
        a = 0;
        b = 2 * block_size;
        while b <= n {
            sym_merge(data, a, a + block_size, b, &less);
            a = b;
            b += 2 * block_size;
        }
        let m = a + block_size;
        if m < n {
            sym_merge(data, a, m, n, &less);
        }
        block_size *= 2;
    }
}

fn insertion_sort<T>(data: &mut [T], a: usize, b: usize, less: &impl Fn(&T, &T) -> bool) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && less(&data[j], &data[j - 1]) {
            data.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn sym_merge<T>(data: &mut [T], a: usize, m: usize, b: usize, less: &impl Fn(&T, &T) -> bool) {
    if m - a == 1 {
        let (mut i, mut j) = (m, b);
        while i < j {
            let h = (i + j) >> 1;
            if less(&data[h], &data[a]) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        for k in a..i.saturating_sub(1) {
            data.swap(k, k + 1);
        }
        return;
    }
    if b - m == 1 {
        let (mut i, mut j) = (a, m);
        while i < j {
            let h = (i + j) >> 1;
            if !less(&data[m], &data[h]) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        let mut k = m;
        while k > i {
            data.swap(k, k - 1);
            k -= 1;
        }
        return;
    }
    let mid = (a + b) >> 1;
    let n = mid + m;
    let (mut start, mut r) = if m > mid { (n - b, mid) } else { (a, m) };
    let p = n - 1;
    while start < r {
        let c = (start + r) >> 1;
        if !less(&data[p - c], &data[c]) {
            start = c + 1;
        } else {
            r = c;
        }
    }
    let end = n - start;
    if start < m && m < end {
        rotate(data, start, m, end);
    }
    if a < start && start < mid {
        sym_merge(data, a, start, mid, less);
    }
    if mid < end && end < b {
        sym_merge(data, mid, end, b, less);
    }
}

fn swap_range<T>(data: &mut [T], a: usize, b: usize, n: usize) {
    for i in 0..n {
        data.swap(a + i, b + i);
    }
}

fn rotate<T>(data: &mut [T], a: usize, m: usize, b: usize) {
    let mut i = m - a;
    let mut j = b - m;
    while i != j {
        if i > j {
            swap_range(data, m - i, m, j);
            i -= j;
        } else {
            swap_range(data, m - i, m + j - i, i);
            j -= i;
        }
    }
    swap_range(data, m - i, m, i);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn go_formats_values_like_fmt_v() {
        let m = json!({"name": "a", "$patch": "x", "n": 8080, "l": ["a", 1, null, true], "m": {"z": 1}});
        assert_eq!(go_v(&m), "map[$patch:x l:[a 1 <nil> true] m:map[z:1] n:8080 name:a]");
        // Printed by Go 1.27's fmt.Printf("%v|", f).
        let floats = [
            1.0,
            80.0,
            1e6,
            1e20,
            1e21,
            123456789.0,
            0.5,
            0.0001,
            0.00001,
            -3.0,
            1.5e300,
            100000.0,
            999999.0,
            1234567.0,
            0.00012345,
        ];
        let printed: Vec<String> = floats.iter().map(|f| go_float(*f)).collect();
        assert_eq!(
            printed.join("|"),
            "1|80|1e+06|1e+20|1e+21|1.23456789e+08|0.5|0.0001|1e-05|-3|1.5e+300|100000|999999|1.234567e+06|0.00012345"
        );
        assert_eq!(go_s(&json!(1)), "%!s(float64=1)");
        assert_eq!(go_s(&json!(true)), "%!s(bool=true)");
    }

    #[test]
    fn stable_sort_matches_go_for_a_strict_order() {
        let mut v: Vec<u32> = (0..100).map(|i| (i * 37) % 23).collect();
        let mut want = v.clone();
        want.sort();
        go_slice_stable(&mut v, |a, b| a < b);
        assert_eq!(v, want);
    }
}
