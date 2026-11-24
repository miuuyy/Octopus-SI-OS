use std::collections::BTreeSet;

use serde_json::{Map, Value};

const MAX_SCHEMA_DEPTH: usize = 32;

const ANNOTATION_KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
    "$defs",
    "definitions",
];

const KNOWN_SEMANTIC_KEYWORDS: &[&str] = &[
    "$ref",
    "type",
    "const",
    "enum",
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "properties",
    "required",
    "additionalProperties",
    "minProperties",
    "maxProperties",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
];

/// Sound, fail-closed JSON Schema narrowing check: returns `true` only when
/// every JSON value accepted by `copy` is provably accepted by `original`
/// (`L(copy) ⊆ L(original)`).
///
/// Any unknown keyword, external or unresolvable `$ref`, cyclic/excessively deep
/// schema, or non-monotone construct without structural equivalence returns
/// `false`.
pub(crate) fn args_schema_narrows(copy: &Value, original: &Value) -> bool {
    if copy == original {
        return true;
    }
    schema_narrows(copy, copy, original, original, 0)
}

fn schema_narrows(
    copy: &Value,
    copy_root: &Value,
    original: &Value,
    orig_root: &Value,
    depth: usize,
) -> bool {
    if depth > MAX_SCHEMA_DEPTH {
        return false;
    }
    if copy == original && (copy_root == orig_root || !contains_ref(copy)) {
        return true;
    }

    // Dereference pure `$ref` nodes (no semantic sibling keywords).
    if let Some(target) = pure_local_ref(copy, copy_root) {
        return schema_narrows(target, copy_root, original, orig_root, depth + 1);
    }
    if let Some(target) = pure_local_ref(original, orig_root) {
        return schema_narrows(copy, copy_root, target, orig_root, depth + 1);
    }

    // If `original` has `$ref` plus sibling keywords, `copy` must narrow both.
    if let Some((orig_ref_target, orig_rest)) = split_ref_and_siblings(original, orig_root) {
        return schema_narrows(copy, copy_root, orig_ref_target, orig_root, depth + 1)
            && schema_narrows(copy, copy_root, &orig_rest, orig_root, depth + 1);
    }
    // Fail closed if `original` carries an unresolvable or non-string `$ref`.
    if original.as_object().is_some_and(|map| map.contains_key("$ref")) {
        return false;
    }

    // Normalize empty semantic object schemas `{}` to `true`.
    let copy_kind = classify_schema(copy);
    let orig_kind = classify_schema(original);

    match (copy_kind, orig_kind) {
        (SchemaKind::Bool(false), SchemaKind::Bool(_) | SchemaKind::Object(_)) => return true,
        (SchemaKind::Bool(_) | SchemaKind::Object(_), SchemaKind::Bool(true)) => return true,
        (_, SchemaKind::Bool(false)) => return false,
        (SchemaKind::Bool(true), SchemaKind::Object(_)) => return false,
        (SchemaKind::Invalid, _) | (_, SchemaKind::Invalid) => return false,
        (SchemaKind::Object(copy_obj), SchemaKind::Object(orig_obj)) => {
            object_schema_narrows(copy_obj, copy_root, orig_obj, orig_root, depth)
        }
    }
}

fn schema_equivalent(
    left: &Value,
    left_root: &Value,
    right: &Value,
    right_root: &Value,
    depth: usize,
) -> bool {
    schema_narrows(left, left_root, right, right_root, depth)
        && schema_narrows(right, right_root, left, left_root, depth)
}

enum SchemaKind<'a> {
    Bool(bool),
    Object(&'a Map<String, Value>),
    Invalid,
}

fn classify_schema(value: &Value) -> SchemaKind<'_> {
    match value {
        Value::Bool(b) => SchemaKind::Bool(*b),
        Value::Object(map) => {
            if has_semantic_keywords(map) {
                SchemaKind::Object(map)
            } else {
                SchemaKind::Bool(true)
            }
        }
        _ => SchemaKind::Invalid,
    }
}

fn is_annotation(key: &str) -> bool {
    ANNOTATION_KEYWORDS.contains(&key)
}

fn is_known_semantic(key: &str) -> bool {
    KNOWN_SEMANTIC_KEYWORDS.contains(&key)
}

fn has_semantic_keywords(map: &Map<String, Value>) -> bool {
    map.keys().any(|k| !is_annotation(k))
}

fn contains_ref(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key("$ref") || map.values().any(contains_ref)
        }
        Value::Array(items) => items.iter().any(contains_ref),
        _ => false,
    }
}

fn resolve_local_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let rest = reference.strip_prefix("#/")?;
    let mut current = root;
    for raw_token in rest.split('/') {
        let token = unescape_json_pointer(raw_token)?;
        match current {
            Value::Object(map) => current = map.get(&token)?,
            Value::Array(items) => {
                let index = token.parse::<usize>().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn unescape_json_pointer(token: &str) -> Option<String> {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn pure_local_ref<'a>(schema: &Value, root: &'a Value) -> Option<&'a Value> {
    let map = schema.as_object()?;
    let reference = map.get("$ref")?.as_str()?;
    if map.keys().any(|k| k != "$ref" && !is_annotation(k)) {
        return None;
    }
    resolve_local_ref(root, reference)
}

fn split_ref_and_siblings<'a>(schema: &Value, root: &'a Value) -> Option<(&'a Value, Value)> {
    let map = schema.as_object()?;
    let reference = map.get("$ref")?.as_str()?;
    let target = resolve_local_ref(root, reference)?;
    let mut rest = Map::new();
    for (k, v) in map {
        if k != "$ref" && !is_annotation(k) {
            rest.insert(k.clone(), v.clone());
        }
    }
    if rest.is_empty() {
        return None;
    }
    Some((target, Value::Object(rest)))
}

fn collect_copy_conjuncts<'a>(
    copy_obj: &'a Map<String, Value>,
    copy_root: &'a Value,
    depth: usize,
) -> Option<Vec<CowSchema<'a>>> {
    if depth > MAX_SCHEMA_DEPTH {
        return None;
    }
    let mut result = Vec::new();
    let mut base = Map::new();
    for (k, v) in copy_obj {
        if is_annotation(k) {
            continue;
        }
        if k == "$ref" {
            let reference = v.as_str()?;
            let target = resolve_local_ref(copy_root, reference)?;
            push_conjunct_value(target, copy_root, depth + 1, &mut result)?;
        } else if k == "allOf" {
            let items = v.as_array()?;
            for item in items {
                push_conjunct_value(item, copy_root, depth + 1, &mut result)?;
            }
        } else {
            base.insert(k.clone(), v.clone());
        }
    }
    if !base.is_empty() {
        result.insert(0, CowSchema::Owned(Value::Object(base)));
    }
    Some(result)
}

fn push_conjunct_value<'a>(
    value: &'a Value,
    root: &'a Value,
    depth: usize,
    out: &mut Vec<CowSchema<'a>>,
) -> Option<()> {
    if depth > MAX_SCHEMA_DEPTH {
        return None;
    }
    match value {
        Value::Bool(true) => Some(()),
        Value::Bool(false) => {
            out.push(CowSchema::Borrowed(value));
            Some(())
        }
        Value::Object(map) => {
            if map.contains_key("$ref") || map.contains_key("allOf") {
                let expanded = collect_copy_conjuncts(map, root, depth)?;
                out.extend(expanded);
                Some(())
            } else if has_semantic_keywords(map) {
                out.push(CowSchema::Borrowed(value));
                Some(())
            } else {
                Some(())
            }
        }
        _ => None,
    }
}

enum CowSchema<'a> {
    Borrowed(&'a Value),
    Owned(Value),
}

impl<'a> CowSchema<'a> {
    fn as_value(&self) -> &Value {
        match self {
            CowSchema::Borrowed(v) => v,
            CowSchema::Owned(v) => v,
        }
    }
}

fn object_schema_narrows(
    copy_obj: &Map<String, Value>,
    copy_root: &Value,
    orig_obj: &Map<String, Value>,
    orig_root: &Value,
    depth: usize,
) -> bool {
    // Reject unknown keywords unless identically matched with no `$ref`s.
    let copy_unknown: BTreeSet<&str> = copy_obj
        .keys()
        .map(String::as_str)
        .filter(|k| !is_annotation(k) && !is_known_semantic(k))
        .collect();
    let orig_unknown: BTreeSet<&str> = orig_obj
        .keys()
        .map(String::as_str)
        .filter(|k| !is_annotation(k) && !is_known_semantic(k))
        .collect();
    if copy_unknown != orig_unknown {
        return false;
    }
    for key in &orig_unknown {
        let (Some(cv), Some(ov)) = (copy_obj.get(*key), orig_obj.get(*key)) else {
            return false;
        };
        if cv != ov || contains_ref(cv) {
            return false;
        }
    }

    // Expand `copy`'s `$ref` + `allOf` conjuncts.
    let Some(conjuncts) = collect_copy_conjuncts(copy_obj, copy_root, depth) else {
        return false;
    };

    // If any conjunct of `copy` is `false`, `L(copy) = ∅ ⊆ L(original)`.
    if conjuncts.iter().any(|c| c.as_value() == &Value::Bool(false)) {
        return true;
    }

    let copy_maps: Vec<&Map<String, Value>> = conjuncts
        .iter()
        .filter_map(|c| c.as_value().as_object())
        .collect();

    // If every branch of a disjunctive `anyOf`/`oneOf` inside `copy` independently narrows
    // the full `original` schema, then `L(copy) ⊆ ⋃ L(branch) ⊆ L(original)`.
    let orig_value = Value::Object(orig_obj.clone());
    if copy_disjunction_narrows(&copy_maps, copy_root, &orig_value, orig_root, depth + 1) {
        return true;
    }

    // 1. `original.allOf`: every branch in `original.allOf` must be narrowed by `copy`.
    if let Some(orig_all_of) = orig_obj.get("allOf") {
        let Some(items) = orig_all_of.as_array() else {
            return false;
        };
        for orig_branch in items {
            let narrowed_by_conjunct = conjuncts.iter().any(|c| {
                schema_narrows(c.as_value(), copy_root, orig_branch, orig_root, depth + 1)
            });
            if !narrowed_by_conjunct
                && !copy_disjunction_narrows(
                    &copy_maps,
                    copy_root,
                    orig_branch,
                    orig_root,
                    depth + 1,
                )
            {
                return false;
            }
        }
    }

    // 2. `original.type`
    if let Some(orig_type) = orig_obj.get("type") {
        let Some(orig_types) = parse_type_set(orig_type) else {
            return false;
        };
        let satisfied = copy_maps.iter().any(|c| copy_satisfies_types(c, &orig_types))
            || copy_disjunction_satisfies(&copy_maps, copy_root, depth, |branch_map| {
                copy_satisfies_types(branch_map, &orig_types)
            });
        if !satisfied {
            return false;
        }
    }

    // 3. `original.const`
    if let Some(orig_const) = orig_obj.get("const") {
        let satisfied = copy_maps.iter().any(|c| copy_satisfies_const(c, orig_const))
            || copy_disjunction_satisfies(&copy_maps, copy_root, depth, |branch_map| {
                copy_satisfies_const(branch_map, orig_const)
            });
        if !satisfied {
            return false;
        }
    }

    // 4. `original.enum`
    if let Some(orig_enum) = orig_obj.get("enum") {
        let Some(allowed) = orig_enum.as_array() else {
            return false;
        };
        let satisfied = copy_maps.iter().any(|c| copy_satisfies_enum(c, allowed))
            || copy_disjunction_satisfies(&copy_maps, copy_root, depth, |branch_map| {
                copy_satisfies_enum(branch_map, allowed)
            });
        if !satisfied {
            return false;
        }
    }

    // 5. Numeric bounds
    if let Some(orig_min) = orig_obj.get("minimum") {
        let Some(o) = orig_min.as_f64().filter(|v| v.is_finite()) else {
            return false;
        };
        if !copy_maps.iter().any(|c| {
            c.get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|v| v.is_finite() && v >= o)
                || c.get("exclusiveMinimum")
                    .and_then(Value::as_f64)
                    .is_some_and(|v| v.is_finite() && v >= o)
        }) {
            return false;
        }
    }
    if let Some(orig_emin) = orig_obj.get("exclusiveMinimum") {
        let Some(o) = orig_emin.as_f64().filter(|v| v.is_finite()) else {
            return false;
        };
        if !copy_maps.iter().any(|c| {
            c.get("exclusiveMinimum")
                .and_then(Value::as_f64)
                .is_some_and(|v| v.is_finite() && v >= o)
                || c.get("minimum")
                    .and_then(Value::as_f64)
                    .is_some_and(|v| v.is_finite() && v > o)
        }) {
            return false;
        }
    }
    if let Some(orig_max) = orig_obj.get("maximum") {
        let Some(o) = orig_max.as_f64().filter(|v| v.is_finite()) else {
            return false;
        };
        if !copy_maps.iter().any(|c| {
            c.get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|v| v.is_finite() && v <= o)
                || c.get("exclusiveMaximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|v| v.is_finite() && v <= o)
        }) {
            return false;
        }
    }
    if let Some(orig_emax) = orig_obj.get("exclusiveMaximum") {
        let Some(o) = orig_emax.as_f64().filter(|v| v.is_finite()) else {
            return false;
        };
        if !copy_maps.iter().any(|c| {
            c.get("exclusiveMaximum")
                .and_then(Value::as_f64)
                .is_some_and(|v| v.is_finite() && v <= o)
                || c.get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|v| v.is_finite() && v < o)
        }) {
            return false;
        }
    }
    if let Some(orig_mult) = orig_obj.get("multipleOf") {
        if !copy_maps
            .iter()
            .any(|c| c.get("multipleOf") == Some(orig_mult))
        {
            return false;
        }
    }

    // 6. String constraints
    if let Some(orig_min_len) = orig_obj.get("minLength") {
        let Some(o) = orig_min_len.as_u64() else {
            return false;
        };
        if !copy_maps
            .iter()
            .any(|c| c.get("minLength").and_then(Value::as_u64).is_some_and(|v| v >= o))
        {
            return false;
        }
    }
    if let Some(orig_max_len) = orig_obj.get("maxLength") {
        let Some(o) = orig_max_len.as_u64() else {
            return false;
        };
        if !copy_maps
            .iter()
            .any(|c| c.get("maxLength").and_then(Value::as_u64).is_some_and(|v| v <= o))
        {
            return false;
        }
    }
    if let Some(orig_pattern) = orig_obj.get("pattern") {
        if !copy_maps
            .iter()
            .any(|c| c.get("pattern") == Some(orig_pattern))
        {
            return false;
        }
    }
    if let Some(orig_format) = orig_obj.get("format") {
        if !copy_maps
            .iter()
            .any(|c| c.get("format") == Some(orig_format))
        {
            return false;
        }
    }

    // 7. Array constraints
    if let Some(orig_min_items) = orig_obj.get("minItems") {
        let Some(o) = orig_min_items.as_u64() else {
            return false;
        };
        if !copy_maps
            .iter()
            .any(|c| c.get("minItems").and_then(Value::as_u64).is_some_and(|v| v >= o))
        {
            return false;
        }
    }
    if let Some(orig_max_items) = orig_obj.get("maxItems") {
        let Some(o) = orig_max_items.as_u64() else {
            return false;
        };
        if !copy_maps
            .iter()
            .any(|c| c.get("maxItems").and_then(Value::as_u64).is_some_and(|v| v <= o))
        {
            return false;
        }
    }
    if let Some(orig_unique) = orig_obj.get("uniqueItems") {
        let Some(o) = orig_unique.as_bool() else {
            return false;
        };
        if o && !copy_maps
            .iter()
            .any(|c| c.get("uniqueItems").and_then(Value::as_bool) == Some(true))
        {
            return false;
        }
    }
    if let Some(orig_items) = orig_obj.get("items") {
        // Tuple-array `items` fails closed unless identically matched without `$ref`.
        if orig_items.is_array() {
            if !copy_maps
                .iter()
                .any(|c| c.get("items") == Some(orig_items) && !contains_ref(orig_items))
            {
                return false;
            }
        } else if !copy_maps.iter().any(|c| {
            c.get("items").is_some_and(|ci| {
                !ci.is_array() && schema_narrows(ci, copy_root, orig_items, orig_root, depth + 1)
            })
        }) {
            return false;
        }
    }

    // 8. Object constraints
    if let Some(orig_min_props) = orig_obj.get("minProperties") {
        let Some(o) = orig_min_props.as_u64() else {
            return false;
        };
        if !copy_maps
            .iter()
            .any(|c| c.get("minProperties").and_then(Value::as_u64).is_some_and(|v| v >= o))
        {
            return false;
        }
    }
    if let Some(orig_max_props) = orig_obj.get("maxProperties") {
        let Some(o) = orig_max_props.as_u64() else {
            return false;
        };
        if !copy_maps
            .iter()
            .any(|c| c.get("maxProperties").and_then(Value::as_u64).is_some_and(|v| v <= o))
        {
            return false;
        }
    }
    if let Some(orig_required) = orig_obj.get("required") {
        let Some(orig_req_set) = parse_string_set(orig_required) else {
            return false;
        };
        // Union of `required` across all conjuncts of `copy` must cover `orig_req_set`.
        let mut copy_req_union = BTreeSet::new();
        for c in &copy_maps {
            if let Some(req) = c.get("required") {
                let Some(set) = parse_string_set(req) else {
                    return false;
                };
                copy_req_union.extend(set);
            }
        }
        if !orig_req_set.is_subset(&copy_req_union) {
            return false;
        }
    }

    // Coupled `properties` + `additionalProperties` check.
    if orig_obj.contains_key("properties") || orig_obj.contains_key("additionalProperties") {
        let satisfied = copy_maps.iter().any(|c| {
            object_properties_narrow(c, copy_root, orig_obj, orig_root, depth)
        });
        if !satisfied {
            return false;
        }
    }

    // 9. `original.anyOf`
    if let Some(orig_any_of) = orig_obj.get("anyOf") {
        let Some(orig_branches) = orig_any_of.as_array() else {
            return false;
        };
        let copy_value = Value::Object(copy_obj.clone());
        let satisfied = orig_branches.iter().any(|ob| {
            schema_narrows(&copy_value, copy_root, ob, orig_root, depth + 1)
                || conjuncts
                    .iter()
                    .any(|c| schema_narrows(c.as_value(), copy_root, ob, orig_root, depth + 1))
        }) || copy_maps.iter().any(|c| {
            for kw in ["anyOf", "oneOf"] {
                if let Some(c_branches) = c.get(kw).and_then(Value::as_array) {
                    if !c_branches.is_empty()
                        && c_branches.iter().all(|cb| {
                            orig_branches.iter().any(|ob| {
                                schema_narrows(cb, copy_root, ob, orig_root, depth + 1)
                            })
                        })
                    {
                        return true;
                    }
                }
            }
            false
        });
        if !satisfied {
            return false;
        }
    }

    // 10. `original.oneOf`: non-monotone uniqueness requires branch-by-branch equivalence.
    if let Some(orig_one_of) = orig_obj.get("oneOf") {
        let Some(orig_branches) = orig_one_of.as_array() else {
            return false;
        };
        let satisfied = copy_maps.iter().any(|c| {
            c.get("oneOf").and_then(Value::as_array).is_some_and(|c_branches| {
                c_branches.len() == orig_branches.len()
                    && c_branches.iter().zip(orig_branches.iter()).all(|(cb, ob)| {
                        schema_equivalent(cb, copy_root, ob, orig_root, depth + 1)
                    })
            })
        });
        if !satisfied {
            return false;
        }
    }

    // 11. `original.not`: contravariant (`L(not C) ⊆ L(not O)` iff `L(O) ⊆ L(C)`).
    if let Some(orig_not) = orig_obj.get("not") {
        let satisfied = copy_maps.iter().any(|c| {
            c.get("not").is_some_and(|copy_not| {
                schema_narrows(orig_not, orig_root, copy_not, copy_root, depth + 1)
            })
        });
        if !satisfied {
            return false;
        }
    }

    // 12. `original.if` / `then` / `else`: non-monotone in `if`; require equivalence.
    if orig_obj.contains_key("if")
        || orig_obj.contains_key("then")
        || orig_obj.contains_key("else")
    {
        let satisfied = copy_maps.iter().any(|c| {
            optional_schema_equivalent(
                c.get("if"),
                copy_root,
                orig_obj.get("if"),
                orig_root,
                depth + 1,
            ) && optional_schema_equivalent(
                c.get("then"),
                copy_root,
                orig_obj.get("then"),
                orig_root,
                depth + 1,
            ) && optional_schema_equivalent(
                c.get("else"),
                copy_root,
                orig_obj.get("else"),
                orig_root,
                depth + 1,
            )
        });
        if !satisfied {
            return false;
        }
    }

    true
}

fn optional_schema_equivalent(
    left: Option<&Value>,
    left_root: &Value,
    right: Option<&Value>,
    right_root: &Value,
    depth: usize,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(l), Some(r)) => schema_equivalent(l, left_root, r, right_root, depth),
        _ => false,
    }
}

fn object_properties_narrow(
    copy_obj: &Map<String, Value>,
    copy_root: &Value,
    orig_obj: &Map<String, Value>,
    orig_root: &Value,
    depth: usize,
) -> bool {
    static TRUE_SCHEMA: Value = Value::Bool(true);
    let empty_props = Map::new();

    let copy_props = match copy_obj.get("properties") {
        None => &empty_props,
        Some(Value::Object(map)) => map,
        Some(_) => return false,
    };
    let orig_props = match orig_obj.get("properties") {
        None => &empty_props,
        Some(Value::Object(map)) => map,
        Some(_) => return false,
    };

    let copy_ap = copy_obj.get("additionalProperties").unwrap_or(&TRUE_SCHEMA);
    let orig_ap = orig_obj.get("additionalProperties").unwrap_or(&TRUE_SCHEMA);

    // Every property allowed by `copy.properties` must narrow its target in `original`
    // (either `orig.properties[k]` or `orig.additionalProperties`).
    for (key, copy_prop_schema) in copy_props {
        let orig_target = orig_props.get(key).unwrap_or(orig_ap);
        if !schema_narrows(copy_prop_schema, copy_root, orig_target, orig_root, depth + 1) {
            return false;
        }
    }

    // Every property present only in `orig.properties` is governed in `copy` by
    // `copy.additionalProperties`. When `copy` sets `additionalProperties: false`,
    // `schema_narrows(false, orig_prop_schema)` succeeds cleanly; when `copy` leaves
    // `additionalProperties: true`, adding a typed property in `original` fails closed.
    for (key, orig_prop_schema) in orig_props {
        if !copy_props.contains_key(key)
            && !schema_narrows(copy_ap, copy_root, orig_prop_schema, orig_root, depth + 1)
        {
            return false;
        }
    }

    // Residual keys not listed in either `properties` map must narrow `orig_ap`.
    schema_narrows(copy_ap, copy_root, orig_ap, orig_root, depth + 1)
}

fn copy_disjunction_narrows(
    copy_maps: &[&Map<String, Value>],
    copy_root: &Value,
    orig_branch: &Value,
    orig_root: &Value,
    depth: usize,
) -> bool {
    copy_maps.iter().any(|c| {
        for kw in ["anyOf", "oneOf"] {
            if let Some(branches) = c.get(kw).and_then(Value::as_array) {
                if !branches.is_empty()
                    && branches
                        .iter()
                        .all(|b| schema_narrows(b, copy_root, orig_branch, orig_root, depth))
                {
                    return true;
                }
            }
        }
        false
    })
}

fn copy_disjunction_satisfies<F>(
    copy_maps: &[&Map<String, Value>],
    copy_root: &Value,
    depth: usize,
    predicate: F,
) -> bool
where
    F: Fn(&Map<String, Value>) -> bool,
{
    if depth >= MAX_SCHEMA_DEPTH {
        return false;
    }
    copy_maps.iter().any(|c| {
        for kw in ["anyOf", "oneOf"] {
            if let Some(branches) = c.get(kw).and_then(Value::as_array) {
                if !branches.is_empty()
                    && branches.iter().all(|b| {
                        let resolved = pure_local_ref(b, copy_root).unwrap_or(b);
                        resolved.as_object().is_some_and(&predicate)
                    })
                {
                    return true;
                }
            }
        }
        false
    })
}

fn parse_type_set(value: &Value) -> Option<BTreeSet<&str>> {
    let mut types = BTreeSet::new();
    match value {
        Value::String(s) => {
            if is_valid_json_type(s) {
                types.insert(s.as_str());
            } else {
                return None;
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                return None;
            }
            for item in items {
                let s = item.as_str()?;
                if !is_valid_json_type(s) {
                    return None;
                }
                types.insert(s);
            }
        }
        _ => return None,
    }
    Some(types)
}

fn is_valid_json_type(ty: &str) -> bool {
    matches!(
        ty,
        "null" | "boolean" | "object" | "array" | "number" | "string" | "integer"
    )
}

fn type_narrows(copy_ty: &str, orig_types: &BTreeSet<&str>) -> bool {
    orig_types.contains(copy_ty) || (copy_ty == "integer" && orig_types.contains("number"))
}

fn copy_satisfies_types(copy_obj: &Map<String, Value>, orig_types: &BTreeSet<&str>) -> bool {
    if let Some(copy_type) = copy_obj.get("type") {
        if let Some(copy_types) = parse_type_set(copy_type) {
            if copy_types.iter().all(|ty| type_narrows(ty, orig_types)) {
                return true;
            }
        }
    }
    if let Some(c) = copy_obj.get("const") {
        if value_matches_any_type(c, orig_types) {
            return true;
        }
    }
    if let Some(Value::Array(values)) = copy_obj.get("enum") {
        if !values.is_empty() && values.iter().all(|v| value_matches_any_type(v, orig_types)) {
            return true;
        }
    }
    false
}

fn value_matches_any_type(value: &Value, types: &BTreeSet<&str>) -> bool {
    match value {
        Value::Null => types.contains("null"),
        Value::Bool(_) => types.contains("boolean"),
        Value::String(_) => types.contains("string"),
        Value::Array(_) => types.contains("array"),
        Value::Object(_) => types.contains("object"),
        Value::Number(n) => {
            types.contains("number")
                || (types.contains("integer")
                    && (n.is_i64()
                        || n.is_u64()
                        || n.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0)))
        }
    }
}

fn copy_satisfies_const(copy_obj: &Map<String, Value>, orig_const: &Value) -> bool {
    if copy_obj.get("const") == Some(orig_const) {
        return true;
    }
    if let Some(Value::Array(values)) = copy_obj.get("enum") {
        if !values.is_empty() && values.iter().all(|v| v == orig_const) {
            return true;
        }
    }
    false
}

fn copy_satisfies_enum(copy_obj: &Map<String, Value>, allowed: &[Value]) -> bool {
    if let Some(c) = copy_obj.get("const") {
        if allowed.iter().any(|a| a == c) {
            return true;
        }
    }
    if let Some(Value::Array(values)) = copy_obj.get("enum") {
        if !values.is_empty() && values.iter().all(|v| allowed.iter().any(|a| a == v)) {
            return true;
        }
    }
    false
}

fn parse_string_set(value: &Value) -> Option<BTreeSet<&str>> {
    let items = value.as_array()?;
    let mut set = BTreeSet::new();
    for item in items {
        set.insert(item.as_str()?);
    }
    Some(set)
}
