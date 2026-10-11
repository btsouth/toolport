//! Client-facing JSON Schema compatibility, compiled once with the tool catalog.
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, BTreeSet};

// Anthropic requires a JSON Schema input_schema:
// https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools
// Its property-key restriction is reported on the official client issue tracker:
// https://github.com/anthropics/claude-code/issues/34771
// ^[a-zA-Z0-9_.-]{1,64}$ permits case, dots and hyphens. In the public
// toolport-mcp-servers@0.2.0 Vercel schemas, the offending keys contain literal
// apostrophes, e.g. "'x-Cwd'". Do not rename the already-valid "x-Cwd".
// Numeric keyword types (including numeric exclusive bounds since draft-06):
// https://json-schema.org/draft/2020-12/json-schema-validation#section-6
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ArgumentMap {
    reference: Option<String>,
    definitions: BTreeMap<String, ArgumentMap>,
    fallback_patterns: Vec<ArgumentMap>,
    fields: BTreeMap<String, (String, ArgumentMap)>,
    items: Option<Box<ArgumentMap>>,
    tuple: Vec<ArgumentMap>,
    branches: Vec<ArgumentMap>,
    extra: Vec<ArgumentMap>,
    patterns: Vec<(regex::Regex, ArgumentMap)>,
    contains: Option<Box<ArgumentMap>>,
    known: BTreeSet<String>,
}

impl ArgumentMap {
    pub(crate) fn is_empty(&self) -> bool {
        self.reference.is_none()
            && self.fallback_patterns.is_empty()
            && self.fields.is_empty()
            && self.items.is_none()
            && self.tuple.is_empty()
            && self.branches.is_empty()
            && self.extra.is_empty()
            && self.patterns.is_empty()
            && self.contains.is_none()
    }

    pub(crate) fn restore(&self, value: &mut Value) -> Result<(), String> {
        self.restore_node(value, &self.definitions, &mut BTreeSet::new())
    }

    fn restore_node(
        &self,
        value: &mut Value,
        definitions: &BTreeMap<String, ArgumentMap>,
        active: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if let Some(reference) = &self.reference {
            // Pure ref cycles must terminate. Descending into an argument child
            // starts a new active set, so recursive objects reuse this map.
            if active.insert(reference.clone()) {
                if let Some(target) = definitions.get(reference) {
                    target.restore_node(value, definitions, active)?;
                }
                active.remove(reference);
            }
            return Ok(());
        }
        if let Some(object) = value.as_object_mut() {
            for (alias, (original, child)) in &self.fields {
                if alias != original && object.contains_key(alias) && object.contains_key(original)
                {
                    return Err(format!(
                        "both schema alias '{alias}' and original key '{original}' were supplied"
                    ));
                }
                if alias != original {
                    if let Some(value) = object.remove(alias) {
                        object.insert(original.clone(), value);
                    }
                }
                if let Some(value) = object.get_mut(original) {
                    child.restore_node(value, definitions, &mut BTreeSet::new())?;
                }
            }
            for (pattern, child) in &self.patterns {
                for (key, value) in object.iter_mut() {
                    if pattern.is_match(key) {
                        child.restore_node(value, definitions, &mut BTreeSet::new())?;
                    }
                }
            }
            for child in &self.fallback_patterns {
                for (key, value) in object.iter_mut() {
                    if !self.known.contains(key) {
                        child.restore_node(value, definitions, &mut BTreeSet::new())?;
                    }
                }
            }
            for extra in &self.extra {
                for (key, value) in object.iter_mut() {
                    if !self.known.contains(key)
                        && !self
                            .patterns
                            .iter()
                            .any(|(pattern, _)| pattern.is_match(key))
                    {
                        extra.restore_node(value, definitions, &mut BTreeSet::new())?;
                    }
                }
            }
        }
        if let Some(array) = value.as_array_mut() {
            for (index, value) in array.iter_mut().enumerate() {
                if let Some(child) = self.tuple.get(index).or(self.items.as_deref()) {
                    child.restore_node(value, definitions, &mut BTreeSet::new())?;
                }
                if let Some(child) = &self.contains {
                    child.restore_node(value, definitions, &mut BTreeSet::new())?;
                }
            }
        }
        for branch in &self.branches {
            branch.restore_node(value, definitions, active)?;
        }
        Ok(())
    }
}

// Visit schema positions only. Values in enum/default/examples and extension
// data are instance data, even when they contain keys named "maximum".
fn children(schema: &mut Map<String, Value>, mut visit: impl FnMut(&mut Value)) {
    for keyword in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(map) = schema.get_mut(keyword).and_then(Value::as_object_mut) {
            for child in map.values_mut() {
                visit(child);
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(array) = schema.get_mut(keyword).and_then(Value::as_array_mut) {
            for child in array {
                visit(child);
            }
        }
    }
    for keyword in [
        "items",
        "additionalItems",
        "additionalProperties",
        "unevaluatedProperties",
        "unevaluatedItems",
        "contentSchema",
        "contains",
        "propertyNames",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(child) = schema.get_mut(keyword) {
            if let Some(array) = child.as_array_mut() {
                for child in array {
                    visit(child);
                }
            } else {
                visit(child);
            }
        }
    }
    if let Some(map) = schema
        .get_mut("dependencies")
        .and_then(Value::as_object_mut)
    {
        for child in map.values_mut().filter(|v| v.is_object()) {
            visit(child);
        }
    }
}

fn collect_keys(schema: &mut Value, valid: &mut BTreeSet<String>, invalid: &mut BTreeSet<String>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for key in properties.keys() {
            if valid_key(key) {
                valid.insert(key.clone());
            } else {
                invalid.insert(key.clone());
            }
        }
    }
    children(object, |child| collect_keys(child, valid, invalid));
}

pub(crate) fn normalize(schema: &mut Value) -> ArgumentMap {
    if has_legacy_bounds(schema) {
        strip_draft04(schema);
    }
    let mut reserved = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    collect_keys(schema, &mut reserved, &mut invalid);
    // One assignment across alternatives makes restoration unambiguous even
    // when the same property is declared in several allOf/oneOf branches.
    let mut aliases = BTreeMap::new();
    for original in invalid {
        let base: String = original
            .trim_matches('\'')
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        let base = if base.is_empty() {
            "arg".to_string()
        } else {
            base
        };
        let mut alias = base.clone();
        let mut index = 2;
        while !reserved.insert(alias.clone()) {
            let suffix = format!("_{index}");
            alias = format!("{}{}", &base[..base.len().min(64 - suffix.len())], suffix);
            index += 1;
        }
        aliases.insert(original, alias);
    }
    // Compile local ref targets before normalization changes pointer keys.
    // Store references by name rather than cyclic owned maps.
    let mut references = BTreeSet::new();
    collect_refs(schema, &mut references);
    let mut definitions = BTreeMap::new();
    for reference in references {
        if let Some(target) = reference.strip_prefix('#').and_then(|p| schema.pointer(p)) {
            definitions.insert(reference, normalize_node(&mut target.clone(), &aliases));
        }
    }
    let mut plan = normalize_node(schema, &aliases);
    plan.definitions = definitions;
    plan
}

fn collect_refs(schema: &mut Value, references: &mut BTreeSet<String>) {
    if let Some(object) = schema.as_object_mut() {
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            references.insert(reference.to_string());
        }
        children(object, |child| collect_refs(child, references));
    }
}

fn has_legacy_bounds(schema: &mut Value) -> bool {
    let Some(object) = schema.as_object_mut() else {
        return false;
    };
    let mut found = ["exclusiveMinimum", "exclusiveMaximum"]
        .iter()
        .any(|key| object.get(*key).is_some_and(Value::is_boolean));
    children(object, |child| found |= has_legacy_bounds(child));
    found
}

fn strip_draft04(schema: &mut Value) {
    if let Some(object) = schema.as_object_mut() {
        if object
            .get("$schema")
            .and_then(Value::as_str)
            .is_some_and(|uri| uri.contains("/draft-04/schema"))
        {
            object.remove("$schema");
        }
        children(object, strip_draft04);
    }
}

fn normalize_number(object: &mut Map<String, Value>, key: &str) {
    let Some(value) = object.get(key) else {
        return;
    };
    let number = value.as_number().cloned().or_else(|| {
        value.as_str().and_then(|s| {
            serde_json::from_str::<Number>(s)
                .ok()
                .or_else(|| s.parse::<f64>().ok().and_then(Number::from_f64))
        })
    });
    let number = number.filter(|number| {
        let parsed = number.as_f64().unwrap_or(f64::NAN);
        // A nonzero mantissa rounded to zero is not representable.
        if parsed == 0.0
            && value.as_str().is_some_and(|s| {
                s.split(['e', 'E'])
                    .next()
                    .unwrap_or(s)
                    .bytes()
                    .any(|b| matches!(b, b'1'..=b'9'))
            })
        {
            return false;
        }
        let value = parsed;
        if matches!(
            key,
            "minLength"
                | "maxLength"
                | "minItems"
                | "maxItems"
                | "minProperties"
                | "maxProperties"
                | "minContains"
                | "maxContains"
        ) {
            value >= 0.0 && value.fract() == 0.0
        } else if key == "multipleOf" {
            value > 0.0
        } else {
            value.is_finite()
        }
    });
    match number {
        Some(number) if !value.is_number() => {
            object.insert(key.to_string(), Value::Number(number));
        }
        Some(_) => {}
        None => {
            object.remove(key);
        }
    }
}

fn rename_list(value: &mut Value, aliases: &BTreeMap<String, String>) {
    if let Some(array) = value.as_array_mut() {
        for value in array {
            if let Some(alias) = value.as_str().and_then(|key| aliases.get(key)) {
                *value = Value::String(alias.clone());
            }
        }
    }
}

fn normalize_node(schema: &mut Value, aliases: &BTreeMap<String, String>) -> ArgumentMap {
    let Some(object) = schema.as_object_mut() else {
        return ArgumentMap::default();
    };
    for key in [
        "minimum",
        "maximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
        "minContains",
        "maxContains",
    ] {
        normalize_number(object, key);
    }
    for (exclusive, inclusive) in [
        ("exclusiveMinimum", "minimum"),
        ("exclusiveMaximum", "maximum"),
    ] {
        if let Some(flag) = object.get(exclusive).and_then(Value::as_bool) {
            object.remove(exclusive);
            if flag {
                if let Some(bound) = object.remove(inclusive) {
                    object.insert(exclusive.to_string(), bound);
                }
            }
        } else {
            normalize_number(object, exclusive);
        }
    }
    let reference = object
        .get("$ref")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(reference) = object.get_mut("$ref") {
        if let Some(pointer) = reference.as_str().and_then(|s| s.strip_prefix("#/")) {
            let mut segments: Vec<String> = pointer.split('/').map(str::to_string).collect();
            let mut changed = false;
            for index in 1..segments.len() {
                if matches!(
                    segments[index - 1].as_str(),
                    "properties" | "dependentSchemas" | "dependencies"
                ) {
                    let key = segments[index].replace("~1", "/").replace("~0", "~");
                    if let Some(alias) = aliases.get(&key) {
                        segments[index] = alias.clone();
                        changed = true;
                    }
                }
            }
            if changed {
                *reference = Value::String(format!("#/{}", segments.join("/")));
            }
        }
    }
    let mut plan = ArgumentMap {
        reference,
        ..ArgumentMap::default()
    };
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        let old = std::mem::take(properties);
        for (original, mut child) in old {
            let alias = aliases.get(&original).unwrap_or(&original).clone();
            let child_plan = normalize_node(&mut child, aliases);
            plan.known.insert(original.clone());
            if alias != original || !child_plan.is_empty() {
                plan.fields.insert(alias.clone(), (original, child_plan));
            }
            properties.insert(alias, child);
        }
    }
    if let Some(required) = object.get_mut("required") {
        rename_list(required, aliases);
    }
    for keyword in ["dependentRequired", "dependencies", "dependentSchemas"] {
        if let Some(map) = object.get_mut(keyword).and_then(Value::as_object_mut) {
            let old = std::mem::take(map);
            for (original, mut child) in old {
                let alias = aliases.get(&original).unwrap_or(&original).clone();
                if child.is_array() {
                    rename_list(&mut child, aliases);
                } else {
                    let branch = normalize_node(&mut child, aliases);
                    if !branch.is_empty() {
                        plan.branches.push(branch);
                    }
                }
                map.insert(alias, child);
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(array) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for child in array {
                let branch = normalize_node(child, aliases);
                if !branch.is_empty() {
                    plan.branches.push(branch);
                }
            }
        }
    }
    for keyword in ["if", "then", "else", "not"] {
        if let Some(child) = object.get_mut(keyword) {
            let branch = normalize_node(child, aliases);
            if !branch.is_empty() {
                plan.branches.push(branch);
            }
        }
    }
    if let Some(array) = object.get_mut("prefixItems").and_then(Value::as_array_mut) {
        plan.tuple = array
            .iter_mut()
            .map(|child| normalize_node(child, aliases))
            .collect();
    }
    if let Some(items) = object.get_mut("items") {
        if let Some(array) = items.as_array_mut() {
            plan.tuple = array
                .iter_mut()
                .map(|child| normalize_node(child, aliases))
                .collect();
        } else {
            let child = normalize_node(items, aliases);
            if !child.is_empty() {
                plan.items = Some(Box::new(child));
            }
        }
    }
    for keyword in ["additionalItems", "unevaluatedItems"] {
        if let Some(child) = object.get_mut(keyword) {
            let child = normalize_node(child, aliases);
            if !child.is_empty() && plan.items.is_none() {
                plan.items = Some(Box::new(child));
            }
        }
    }
    for keyword in ["additionalProperties", "unevaluatedProperties"] {
        if let Some(child) = object.get_mut(keyword) {
            let child = normalize_node(child, aliases);
            if !child.is_empty() {
                plan.extra.push(child);
            }
        }
    }
    // These schemas do not describe a fixed argument position, but still need
    // valid numeric keywords and property declarations on the published side.
    if let Some(map) = object
        .get_mut("patternProperties")
        .and_then(Value::as_object_mut)
    {
        for (pattern, child) in map {
            let child = normalize_node(child, aliases);
            // JSON Schema supports lookaround that Rust regex cannot compile.
            // Restore the child map for otherwise unknown keys in that case.
            if !child.is_empty() {
                match regex::Regex::new(pattern) {
                    Ok(pattern) => plan.patterns.push((pattern, child)),
                    Err(_) => plan.fallback_patterns.push(child),
                }
            }
        }
    }
    if let Some(child) = object.get_mut("contains") {
        let child = normalize_node(child, aliases);
        if !child.is_empty() {
            plan.contains = Some(Box::new(child));
        }
    }
    for keyword in ["$defs", "definitions"] {
        if let Some(map) = object.get_mut(keyword).and_then(Value::as_object_mut) {
            for child in map.values_mut() {
                normalize_node(child, aliases);
            }
        }
    }
    for keyword in ["contentSchema"] {
        if let Some(child) = object.get_mut(keyword) {
            let child = normalize_node(child, aliases);
            if !child.is_empty() {
                plan.branches.push(child);
            }
        }
    }
    for keyword in ["propertyNames"] {
        if let Some(child) = object.get_mut(keyword) {
            normalize_node(child, aliases);
        }
    }
    if plan.extra.is_empty() && plan.fallback_patterns.is_empty() {
        plan.known.clear();
    }
    if plan.tuple.iter().all(ArgumentMap::is_empty) && plan.items.is_none() {
        plan.tuple.clear();
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn review_lookahead_pattern_restores_unknown_properties() {
        let mut schema = json!({
            "properties": {"fixed": {}},
            "patternProperties": {"^(?!internal)": {"properties": {"a b": {}}}}
        });
        let plan = normalize(&mut schema);
        let mut args = json!({"fixed": {"a_b": 1}, "dynamic": {"a_b": 2}});
        plan.restore(&mut args).unwrap();
        assert_eq!(args, json!({"fixed": {"a_b": 1}, "dynamic": {"a b": 2}}));
    }

    fn check_new_position(keyword: &str, mut args: Value) {
        let mut schema = json!({keyword: {"properties": {"a b": {"minimum": "3"}}}});
        let plan = normalize(&mut schema);
        assert_eq!(schema[keyword]["properties"]["a_b"]["minimum"], 3);
        plan.restore(&mut args).unwrap();
        assert!(!args.to_string().contains("a_b"));
    }

    #[test]
    fn review_unevaluated_properties() {
        check_new_position("unevaluatedProperties", json!({"dynamic": {"a_b": 1}}));
    }

    #[test]
    fn review_unevaluated_items() {
        check_new_position("unevaluatedItems", json!([{"a_b": 1}]));
    }

    #[test]
    fn review_content_schema() {
        check_new_position("contentSchema", json!({"a_b": 1}));
    }

    #[test]
    fn review_ref_only_cycles_terminate() {
        let mut schema = json!({"$ref": "#/$defs/A", "$defs": {
            "A": {"allOf": [{"$ref": "#/$defs/B"}]},
            "B": {"$ref": "#/$defs/A"}
        }});
        let plan = normalize(&mut schema);
        let mut args = json!({"untouched": 1});
        plan.restore(&mut args).unwrap();
        assert_eq!(args, json!({"untouched": 1}));
    }

    #[test]
    fn review_draft04_declaration() {
        let mut schema = json!({"$schema": "http://json-schema.org/draft-04/schema#",
            "minimum": 3, "exclusiveMinimum": true});
        normalize(&mut schema);
        assert!(schema.get("$schema").is_none());
    }

    #[test]
    fn review_numeric_underflow() {
        let mut schema = json!({"minimum": "1e-400"});
        normalize(&mut schema);
        assert!(schema.get("minimum").is_none());
    }

    #[test]
    fn draft04_exclusive_bounds_become_numeric() {
        for (inclusive, exclusive) in [
            ("minimum", "exclusiveMinimum"),
            ("maximum", "exclusiveMaximum"),
        ] {
            let mut schema = json!({inclusive: "3", exclusive: true});
            normalize(&mut schema);
            assert_eq!(schema, json!({exclusive: 3}));
            let mut schema = json!({inclusive: 3, exclusive: false});
            normalize(&mut schema);
            assert_eq!(schema, json!({inclusive: 3}));
            let mut schema = json!({exclusive: true});
            normalize(&mut schema);
            assert_eq!(schema, json!({}));
        }
    }

    #[test]
    fn numeric_strings_and_invalid_bounds() {
        for key in [
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "minProperties",
            "maxProperties",
            "minContains",
            "maxContains",
        ] {
            let mut schema = json!({key: "9e+07"});
            normalize(&mut schema);
            assert_eq!(schema[key].as_f64(), Some(90_000_000.0));
            for invalid in [
                json!("NaN"),
                json!("Infinity"),
                json!("1e999"),
                json!("invalid"),
                json!(null),
                json!([]),
                json!(false),
            ] {
                let mut schema = json!({key: invalid});
                normalize(&mut schema);
                assert!(schema.get(key).is_none(), "{key}: {schema}");
            }
        }
        let mut schema = json!({"maximum": "18446744073709551615"});
        normalize(&mut schema);
        assert_eq!(schema["maximum"].as_u64(), Some(u64::MAX));
    }

    #[test]
    fn count_bounds_stay_nonnegative_integers_and_multiple_of_positive() {
        for key in [
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "minProperties",
            "maxProperties",
            "minContains",
            "maxContains",
        ] {
            for value in [json!("-1"), json!("2.5"), json!(-1), json!(2.5)] {
                let mut schema = json!({key: value});
                normalize(&mut schema);
                assert!(schema.get(key).is_none(), "{schema}");
            }
        }
        for value in [json!("0"), json!("-1"), json!(0)] {
            let mut schema = json!({"multipleOf": value});
            normalize(&mut schema);
            assert!(schema.get("multipleOf").is_none());
        }
    }

    #[test]
    fn valid_schemas_are_byte_identical_including_instance_data() {
        let mut schema = json!({
            "type": "object", "properties": {
                "x-Artifact-Client-Ci": {"type": "string", "minLength": 0, "maxLength": 64},
                "content-Length": {"type": "number", "maximum": 50, "exclusiveMinimum": 0},
                "name.exact": {"type": "string"},
                "body": {"type": "object", "default": {"maximum": "NaN", "'quoted'": 1}, "enum": [{"minimum": "3"}]}
            }, "required": ["content-Length"], "examples": [{"'x-Cwd'": "/tmp"}],
            "x-extension": {"minimum": "NaN", "properties": {"'a'": {}}}
        });
        let bytes = serde_json::to_vec(&schema).unwrap();
        let plan = normalize(&mut schema);
        assert_eq!(serde_json::to_vec(&schema).unwrap(), bytes);
        assert!(plan.is_empty());
    }

    #[test]
    fn aliases_preserve_required_and_avoid_collisions_deterministically() {
        let original = json!({"properties": {"'x-Cwd'": {}, "x-Cwd": {}, "x-Cwd_2": {}, "": {}, "$.xgafv": {}, "é": {}, "a b": {}, "a_b": {}}, "required": ["'x-Cwd'", "a b"], "dependentRequired": {"'x-Cwd'": ["a b"]}});
        let mut schema = original.clone();
        let plan = normalize(&mut schema);
        assert_eq!(schema["required"], json!(["x-Cwd_3", "a_b_2"]));
        assert_eq!(schema["dependentRequired"]["x-Cwd_3"], json!(["a_b_2"]));
        assert!(schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .all(|k| valid_key(k)));
        let mut args = json!({"x-Cwd_3": 1, "x-Cwd": 2, "a_b_2": 3, "a_b": 4});
        plan.restore(&mut args).unwrap();
        assert_eq!(args, json!({"'x-Cwd'": 1, "x-Cwd": 2, "a b": 3, "a_b": 4}));
        let mut again = original;
        normalize(&mut again);
        assert_eq!(schema, again);
        let bytes = serde_json::to_vec(&schema).unwrap();
        normalize(&mut schema);
        assert_eq!(serde_json::to_vec(&schema).unwrap(), bytes);
    }

    #[test]
    fn local_property_references_follow_renamed_keys() {
        let mut schema = json!({"properties": {"a/b": {"type": "string"}, "copy": {"$ref": "#/properties/a~1b"}}});
        normalize(&mut schema);
        assert_eq!(schema["properties"]["copy"]["$ref"], "#/properties/a_b");
    }

    #[test]
    fn long_keys_have_unique_bounded_aliases() {
        let a = "a".repeat(65);
        let b = format!("{}b", "a".repeat(64));
        let valid = "a".repeat(64);
        let mut schema = json!({"properties": {a: {}, b: {}, valid: {}}});
        normalize(&mut schema);
        let keys = schema["properties"].as_object().unwrap();
        assert_eq!(keys.len(), 3);
        assert!(keys.keys().all(|k| valid_key(k)));
    }

    #[test]
    fn restores_nested_arrays_and_composed_schemas_without_touching_free_data() {
        let mut schema = json!({"properties": {
            "body": {"type": "array", "items": {"allOf": [
                {"properties": {"'x-Cwd'": {"type": "string"}}},
                {"properties": {"'x-Cwd'": {"type": "string"}, "nested": {"properties": {"a b": {}}}}}
            ]}}, "free": {"type": "object"}
        }});
        let plan = normalize(&mut schema);
        let mut args = json!({"body": [{"x-Cwd": "/tmp", "nested": {"a_b": 3}}], "free": {"x-Cwd": "leave alone"}});
        plan.restore(&mut args).unwrap();
        assert_eq!(
            args,
            json!({"body": [{"'x-Cwd'": "/tmp", "nested": {"a b": 3}}], "free": {"x-Cwd": "leave alone"}})
        );
    }

    #[test]
    fn restores_tuple_pattern_and_additional_property_values() {
        let mut schema = json!({"properties": {
            "tuple": {"prefixItems": [{"properties": {"a b": {}}}], "items": {"properties": {"'x-Cwd'": {}}}},
            "map": {"patternProperties": {"^known": {"properties": {"a b": {}}}}, "additionalProperties": {"properties": {"'x-Cwd'": {}}}}
        }});
        let plan = normalize(&mut schema);
        let mut args = json!({"tuple": [{"a_b": 1}, {"x-Cwd": 2}], "map": {"known-key": {"a_b": 3}, "other-key": {"x-Cwd": 4}}});
        plan.restore(&mut args).unwrap();
        assert_eq!(
            args,
            json!({"tuple": [{"a b": 1}, {"'x-Cwd'": 2}], "map": {"known-key": {"a b": 3}, "other-key": {"'x-Cwd'": 4}}})
        );
    }

    #[test]
    fn refuses_alias_and_original_instead_of_overwriting() {
        let mut schema = json!({"properties": {"'x-Cwd'": {}}});
        let plan = normalize(&mut schema);
        let mut args = json!({"x-Cwd": 1, "'x-Cwd'": 2});
        assert!(plan
            .restore(&mut args)
            .unwrap_err()
            .contains("both schema alias"));
    }

    #[test]
    fn cloudflare_string_maximum_is_numeric_only_in_schema_positions() {
        let mut schema = json!({
            "type":"object",
            "properties":{"per_page":{"type":"integer", "minimum":"1", "maximum":"100"}},
            "$defs":{"limits":{"minItems":"0", "maxItems":"100", "maxProperties":"invalid"}},
            "examples":[{"maximum":"100"}],
        });
        normalize(&mut schema);
        assert_eq!(schema["properties"]["per_page"]["maximum"], 100);
        assert_eq!(schema["properties"]["per_page"]["minimum"], 1);
        assert_eq!(schema["$defs"]["limits"]["maxItems"], 100);
        assert!(schema["$defs"]["limits"].get("maxProperties").is_none());
        assert_eq!(schema["examples"][0]["maximum"], "100");
    }

    #[test]
    fn normalizes_public_preview2_schemas() {
        // Exact inputSchema objects from toolport-mcp-servers@0.2.0's shipped
        // data/{clerk,cloudflare,vercel}-curated.tools.json. No installed registry.
        let mut tools: Vec<Value> =
            serde_json::from_str(include_str!("../tests/fixtures/schema-compat-public.json"))
                .unwrap();
        normalize(&mut tools[0]["inputSchema"]);
        let clerk = &tools[0]["inputSchema"]["properties"]["body"]["properties"]
            ["seconds_until_expiration"];
        assert_eq!(clerk["exclusiveMinimum"], 0);
        assert!(clerk.get("minimum").is_none());
        normalize(&mut tools[1]["inputSchema"]);
        assert_eq!(
            tools[1]["inputSchema"]["properties"]["per_page"]["maximum"].as_f64(),
            Some(5_000_000.0)
        );
        let plan = normalize(&mut tools[2]["inputSchema"]);
        assert!(tools[2]["inputSchema"]["properties"]
            .get("'x-Cwd'")
            .is_none());
        let mut args = json!({"sessionId": "sbx_fixture", "x-Cwd": "/tmp"});
        plan.restore(&mut args).unwrap();
        assert_eq!(args["'x-Cwd'"], "/tmp");
    }
}
