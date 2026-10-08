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
    fields: BTreeMap<String, (String, ArgumentMap)>,
    items: Option<Box<ArgumentMap>>,
    tuple: Vec<ArgumentMap>,
    branches: Vec<ArgumentMap>,
    extra: Option<Box<ArgumentMap>>,
    patterns: Vec<(regex::Regex, ArgumentMap)>,
    contains: Option<Box<ArgumentMap>>,
    known: BTreeSet<String>,
}

impl ArgumentMap {
    pub(crate) fn is_empty(&self) -> bool {
        self.fields.is_empty()
            && self.items.is_none()
            && self.tuple.is_empty()
            && self.branches.is_empty()
            && self.extra.is_none()
            && self.patterns.is_empty()
            && self.contains.is_none()
    }

    pub(crate) fn restore(&self, value: &mut Value) -> Result<(), String> {
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
                    child.restore(value)?;
                }
            }
            for (pattern, child) in &self.patterns {
                for (key, value) in object.iter_mut() {
                    if pattern.is_match(key) {
                        child.restore(value)?;
                    }
                }
            }
            if let Some(extra) = &self.extra {
                for (key, value) in object.iter_mut() {
                    if !self.known.contains(key)
                        && !self
                            .patterns
                            .iter()
                            .any(|(pattern, _)| pattern.is_match(key))
                    {
                        extra.restore(value)?;
                    }
                }
            }
        }
        if let Some(array) = value.as_array_mut() {
            for (index, value) in array.iter_mut().enumerate() {
                if let Some(child) = self.tuple.get(index).or(self.items.as_deref()) {
                    child.restore(value)?;
                }
                if let Some(child) = &self.contains {
                    child.restore(value)?;
                }
            }
        }
        for branch in &self.branches {
            branch.restore(value)?;
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
    normalize_node(schema, &aliases)
}

fn normalize_number(object: &mut Map<String, Value>, key: &str) {
    let Some(value) = object.get(key) else {
        return;
    };
    if value.is_number() {
        return;
    }
    let number = value.as_str().and_then(|s| {
        serde_json::from_str::<Number>(s)
            .ok()
            .or_else(|| s.parse::<f64>().ok().and_then(Number::from_f64))
    });
    match number {
        Some(number) => {
            object.insert(key.to_string(), Value::Number(number));
        }
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
    let mut plan = ArgumentMap::default();
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
    if plan.items.is_none() {
        if let Some(child) = object.get_mut("additionalItems") {
            let child = normalize_node(child, aliases);
            if !child.is_empty() {
                plan.items = Some(Box::new(child));
            }
        }
    }
    if let Some(child) = object.get_mut("additionalProperties") {
        let child = normalize_node(child, aliases);
        if !child.is_empty() {
            plan.extra = Some(Box::new(child));
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
            // JSON Schema already requires valid regexes. Keep invalid regexes
            // unchanged; they cannot define a safe argument mapping.
            if !child.is_empty() {
                if let Ok(pattern) = regex::Regex::new(pattern) {
                    plan.patterns.push((pattern, child));
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
    for keyword in ["propertyNames"] {
        if let Some(child) = object.get_mut(keyword) {
            normalize_node(child, aliases);
        }
    }
    if plan.extra.is_none() {
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
