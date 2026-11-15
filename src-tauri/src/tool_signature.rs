//! Compact TypeScript-style signatures for Code Mode.
//!
//! A search hit tells a script author how to call a tool from `toolport_run_script`:
//! `servers.github.create_issue(args: {body?: string; title: string}): ToolResult`.
//!
//! The rule every signature obeys: a script written from it is valid against the
//! tool's JSON Schema. A signature may be wider than the schema (`string` for a
//! pattern, `unknown` for a construct TypeScript cannot spell), but it never shows a
//! required argument as optional or absent. To get there the renderer first merges
//! each schema into an effective shape: `allOf` branches and `$ref` targets are
//! merged (properties, `required`, `additionalProperties`, `patternProperties`,
//! array bounds), `anyOf`/`oneOf` distribute over that merge into a union of merged
//! shapes, and `args` is only optional when the effective schema accepts `{}`.
//!
//! The text comes from upstream JSON Schema, which is untrusted, so the renderer is
//! total and bounded by work, not by depth: it counts nodes visited and output
//! characters. `$ref` targets are memoised, so repeated references cost nothing;
//! recursive references become named declarations. Native recursion stays shallow
//! because anything nested past [`INLINE_DEPTH`] becomes a named declaration too,
//! rendered later from a work list.
//!
//! A search hit always carries a signature of at most [`MAX_SIGNATURE_CHARS`]. A
//! longer one is shortened in steps, each still a true (if looser) type: nested
//! objects become `object`, the typed return becomes plain `ToolResult`, optional
//! parameters fold into `...+N optional`, enums become their base type, and required
//! parameters that still do not fit keep their names with an `unknown` type or fold
//! into `...+N required`. `toolport.describe(name)` returns [`full_signature`]
//! instead, which is never shortened; when a guard is hit it says so.
//!
//! Parameter descriptions are left out on purpose; the hit already carries the tool
//! description, and the signature only has to carry shape.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value};

use crate::integrity;

/// Longest signature a search hit carries. A longer one is shortened in steps, never
/// cut mid-type, because a cut type is a wrong type.
pub const MAX_SIGNATURE_CHARS: usize = 240;

/// Guard on the text [`full_signature`] returns. Half the code mode worker frame, so
/// the signature and the schema returned next to it fit one frame together for any
/// schema an upstream can realistically publish. Reaching it is reported, not hidden.
pub const MAX_DESCRIBE_SIGNATURE_BYTES: usize = 8 * 1024 * 1024;

/// Schema nodes one search-hit signature may visit. Past it, deeper types read
/// `unknown` (wider, so still true); keys and `required` already read stay exact.
const SEARCH_WORK: usize = 10_000;
/// Schema nodes [`full_signature`] may visit before it reports `truncated`.
const DESCRIBE_WORK: usize = 4_000_000;
/// Nesting rendered inline before a subschema becomes a named declaration. Keeps
/// native recursion shallow; it is not a cap on what gets described.
const INLINE_DEPTH: usize = 48;
/// `describe` inlines shared definitions until the text passes this size, then
/// renders every shared definition once as a named declaration instead.
const DESCRIBE_INLINE_CHARS: usize = 256 * 1024;
/// Tuple positions spelled out to express `minItems`/`maxItems` beyond `prefixItems`.
/// Larger bounds widen to a plain rest element, which is still true.
const MAX_SPELLED_POSITIONS: u64 = 64;

/// Rendering stopped because the text exceeded its budget.
#[derive(Debug, PartialEq, Eq)]
struct TooLong;

/// JSON value kinds as a bit set. `integer` folds into `number`.
type Kinds = u8;
const K_NULL: Kinds = 1;
const K_BOOL: Kinds = 2;
const K_NUM: Kinds = 4;
const K_STR: Kinds = 8;
const K_ARR: Kinds = 16;
const K_OBJ: Kinds = 32;
const K_ALL: Kinds = 63;

fn kind_of(value: &Value) -> Kinds {
    match value {
        Value::Null => K_NULL,
        Value::Bool(_) => K_BOOL,
        Value::Number(_) => K_NUM,
        Value::String(_) => K_STR,
        Value::Array(_) => K_ARR,
        Value::Object(_) => K_OBJ,
    }
}

fn kinds_of_name(name: &str) -> Kinds {
    match name {
        "null" => K_NULL,
        "boolean" => K_BOOL,
        "integer" | "number" => K_NUM,
        "string" => K_STR,
        "array" => K_ARR,
        "object" => K_OBJ,
        // An unknown type name constrains nothing we can render.
        _ => K_ALL,
    }
}

/// The kinds one schema object's `type` (plus OpenAPI `nullable`) allows.
fn part_kinds(part: &Map<String, Value>) -> Kinds {
    let kinds = match part.get("type") {
        Some(Value::String(name)) => kinds_of_name(name),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .fold(0, |kinds, name| kinds | kinds_of_name(name)),
        _ => K_ALL,
    };
    if part.get("nullable") == Some(&Value::Bool(true)) {
        kinds | K_NULL
    } else {
        kinds
    }
}

/// Keywords that only annotate: a `$ref` next to them is a plain reference.
fn is_annotation(key: &str) -> bool {
    matches!(
        key,
        "$ref"
            | "description"
            | "title"
            | "default"
            | "examples"
            | "$comment"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
            | "$id"
            | "$anchor"
            | "$schema"
            | "$defs"
            | "definitions"
    )
}

fn has_object_keywords(part: &Map<String, Value>) -> bool {
    [
        "properties",
        "additionalProperties",
        "patternProperties",
        "required",
    ]
    .iter()
    .any(|key| part.contains_key(*key))
}

fn has_array_keywords(part: &Map<String, Value>) -> bool {
    ["items", "prefixItems", "minItems", "maxItems"]
        .iter()
        .any(|key| part.contains_key(*key))
}

static NULL: Value = Value::Null;

fn address(value: &Value) -> usize {
    value as *const Value as usize
}

/// A rendered type, before printing. Borrowed from the schema it came from.
#[derive(Clone, Debug)]
enum Ty<'a> {
    Unknown,
    Never,
    Prim(&'static str),
    /// A `const` or `enum` value, printed as a literal type.
    Lit(&'a Value),
    Array(Box<Ty<'a>>),
    Tuple {
        items: Vec<Ty<'a>>,
        required: usize,
        rest: Option<Box<Ty<'a>>>,
    },
    Object {
        props: Vec<Prop<'a>>,
        index: Index<'a>,
    },
    Union(Vec<Ty<'a>>),
    /// A union admitting every kind (an untyped nested schema). Printed in full,
    /// or as `unknown` once shortening collapses nested types.
    Wide(Vec<Ty<'a>>),
    /// A memoised definition, inlined or printed by name.
    Ref(usize),
}

#[derive(Clone, Debug)]
struct Prop<'a> {
    key: &'a str,
    required: bool,
    ty: Ty<'a>,
}

/// What an object says about keys it does not name.
#[derive(Clone, Debug)]
enum Index<'a> {
    /// Unconstrained: `Record<string, unknown>` when there are no named keys.
    Free,
    /// `additionalProperties: false` with no patterns.
    Closed,
    Typed(Box<Ty<'a>>),
}

impl Ty<'_> {
    fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }
}

fn union<'a>(members: Vec<Ty<'a>>) -> Ty<'a> {
    let mut flat: Vec<Ty<'a>> = Vec::with_capacity(members.len());
    let mut wide = false;
    for member in members {
        match member {
            Ty::Unknown => return Ty::Unknown,
            Ty::Never => {}
            Ty::Union(inner) => flat.extend(inner),
            Ty::Wide(inner) => {
                wide = true;
                flat.extend(inner);
            }
            other => flat.push(other),
        }
    }
    match flat.len() {
        0 => Ty::Never,
        1 => flat.pop().unwrap(),
        _ if wide => Ty::Wide(flat),
        _ => Ty::Union(flat),
    }
}

/// One memoised definition: a `$ref` target, or a subschema nested past
/// [`INLINE_DEPTH`] that is rendered later as a named declaration.
struct Entry<'a> {
    name: String,
    root: &'a Value,
    subs: Vec<&'a Value>,
    ty: Option<Ty<'a>>,
    building: bool,
    /// Reached again while it was being built.
    recursive: bool,
    /// Built from the work list and always printed by name.
    named: bool,
    uses: usize,
}

/// A choice a merged shape still has to make. Each becomes a union of shapes.
#[derive(Clone, Copy)]
enum Group<'a> {
    /// `anyOf` or `oneOf` (true): one member holds.
    Any(&'a [Value], bool),
    /// `if`/`then`/`else`. TypeScript cannot test the condition, so this renders
    /// as the union of the then-shape (with the condition merged in) and the
    /// else-shape, unless the condition is settled for the shape at hand.
    Cond {
        cond: &'a Value,
        then: Option<&'a Value>,
        otherwise: Option<&'a Value>,
    },
}

/// The merged view of a list of schemas that must all hold (`allOf` semantics).
#[derive(Clone, Default)]
struct Shape<'a> {
    parts: Vec<&'a Map<String, Value>>,
    /// Choices not yet distributed.
    groups: Vec<Group<'a>>,
    never: bool,
    /// Holds a constraint the renderer cannot see (an external or dynamic `$ref`).
    /// Only [`Ctx::accepts_empty`] cares: the rendered type is still a true widening.
    opaque: bool,
    seen: HashSet<usize>,
    /// Keys required by an applied `dependentRequired` or a trigger taken as present.
    extra_required: Vec<&'a str>,
    /// Trigger keys taken as absent in this branch of a dependency.
    absent: Vec<&'a str>,
    /// Dependencies already applied, by address.
    applied: HashSet<usize>,
    /// Kinds this branch rules out. Object keywords (`required`, dependencies, a
    /// key taken as absent) constrain only objects, so a branch that needs the
    /// value to be an object excludes every other kind, and one that keeps the
    /// non-object values excludes objects, instead of either becoming `never`.
    exclude: Kinds,
}

/// What failing a simple `if` implies about one key.
#[derive(Clone, Copy)]
enum Refinement<'a> {
    /// `if: {properties: {k: ...}}` fails only when `k` is present (and fails).
    Present(&'a str),
    /// `if: {required: [k]}` fails only when `k` is absent.
    Absent(&'a str),
}

/// For a condition about exactly one key, what an instance failing it must do.
fn failing(cond: &Value) -> Option<Refinement<'_>> {
    let object = cond.as_object()?;
    let mut assertions = object
        .iter()
        .filter(|(key, _)| !is_annotation(key) || *key == "$ref");
    let (keyword, value) = assertions.next()?;
    if assertions.next().is_some() {
        return None;
    }
    match (keyword.as_str(), value) {
        ("properties", Value::Object(properties)) if properties.len() == 1 => properties
            .keys()
            .next()
            .map(|key| Refinement::Present(key.as_str())),
        ("required", Value::Array(keys)) if keys.len() == 1 => {
            keys[0].as_str().map(Refinement::Absent)
        }
        _ => None,
    }
}

/// A dependency keyword entry: its trigger key and what it adds.
#[derive(Clone, Copy)]
enum Dependency<'a> {
    Required(&'a [Value]),
    Schema(&'a Value),
}

/// Three-valued answer for "does the schema accept `{}`".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    Yes,
    No,
    Maybe,
}

struct Ctx<'a> {
    root: &'a Value,
    work: usize,
    max_work: usize,
    exhausted: bool,
    entries: Vec<Entry<'a>>,
    by_key: HashMap<Vec<usize>, usize>,
    pending: Vec<usize>,
    names: HashSet<String>,
    regex: HashMap<&'a str, Option<regex::Regex>>,
    empty_memo: HashMap<usize, Tri>,
}

impl<'a> Ctx<'a> {
    fn new(root: &'a Value, max_work: usize) -> Self {
        Ctx {
            root,
            work: 0,
            max_work,
            exhausted: false,
            entries: Vec::new(),
            by_key: HashMap::new(),
            pending: Vec::new(),
            names: HashSet::new(),
            regex: HashMap::new(),
            empty_memo: HashMap::new(),
        }
    }

    /// Spend one unit of work. `false` once the budget is gone.
    fn tick(&mut self) -> bool {
        if self.work >= self.max_work {
            self.exhausted = true;
            return false;
        }
        self.work += 1;
        true
    }

    fn resolve(&self, reference: &str) -> Option<&'a Value> {
        let pointer = reference.strip_prefix('#')?;
        if pointer.is_empty() {
            Some(self.root)
        } else {
            self.root.pointer(pointer)
        }
    }

    /// A schema that is only a local `$ref` (plus annotations): its target.
    /// `Err(())` for a reference that cannot be resolved here.
    fn plain_ref(&self, schema: &'a Value) -> Option<Result<(&'a Value, &'a str), ()>> {
        let object = schema.as_object()?;
        let reference = object.get("$ref")?.as_str()?;
        if !object.keys().all(|key| is_annotation(key)) {
            return None;
        }
        Some(
            self.resolve(reference)
                .map(|target| (target, reference))
                .ok_or(()),
        )
    }

    /// Merge `sub` into `shape`, following `allOf` and `$ref` with an explicit stack.
    fn flatten(&mut self, shape: &mut Shape<'a>, sub: &'a Value) {
        let mut stack = vec![sub];
        while let Some(sub) = stack.pop() {
            if !self.tick() {
                shape.opaque = true;
                return;
            }
            let object = match sub {
                Value::Bool(true) => continue,
                Value::Bool(false) => {
                    shape.never = true;
                    continue;
                }
                Value::Object(object) => object,
                // Not a schema; it constrains nothing we can render.
                _ => {
                    shape.opaque = true;
                    continue;
                }
            };
            // A schema already merged adds nothing to a conjunction.
            if !shape.seen.insert(address(sub)) {
                continue;
            }
            shape.parts.push(object);
            if let Some(reference) = object.get("$ref") {
                match reference
                    .as_str()
                    .and_then(|reference| self.resolve(reference))
                {
                    Some(target) => stack.push(target),
                    None => shape.opaque = true,
                }
            }
            if object.contains_key("$dynamicRef") || object.contains_key("$recursiveRef") {
                shape.opaque = true;
            }
            if let Some(Value::Array(members)) = object.get("allOf") {
                stack.extend(members.iter().rev());
            }
            for (key, one_of) in [("anyOf", false), ("oneOf", true)] {
                match object.get(key) {
                    Some(Value::Array(members)) if members.is_empty() => shape.never = true,
                    Some(Value::Array(members)) => shape.groups.push(Group::Any(members, one_of)),
                    _ => {}
                }
            }
            if let Some(cond) = object.get("if") {
                let (then, otherwise) = (object.get("then"), object.get("else"));
                if then.is_some() || otherwise.is_some() {
                    shape.groups.push(Group::Cond {
                        cond,
                        then,
                        otherwise,
                    });
                }
            }
        }
    }

    fn shape_of(&mut self, subs: &[&'a Value]) -> Shape<'a> {
        let mut shape = Shape::default();
        for sub in subs {
            self.flatten(&mut shape, sub);
        }
        shape
    }

    /// Required keys of a merged shape: every part's `required` plus keys an
    /// applied dependency added. Dependencies were expanded before this.
    fn required_keys(&mut self, shape: &Shape<'a>) -> BTreeSet<&'a str> {
        let mut required: BTreeSet<&'a str> = shape.extra_required.iter().copied().collect();
        let mut scanned = shape.parts.len();
        for part in &shape.parts {
            if let Some(Value::Array(keys)) = part.get("required") {
                scanned += keys.len();
                required.extend(keys.iter().filter_map(Value::as_str));
            }
        }
        self.charge(scanned);
        required
    }

    /// Whether `key` is certainly present in a shape: required by a part or by an
    /// applied dependency.
    fn certainly_present(&mut self, shape: &Shape<'a>, key: &str) -> bool {
        if shape.extra_required.contains(&key) {
            return true;
        }
        self.charge(shape.parts.len());
        shape.parts.iter().any(|part| {
            part.get("required")
                .and_then(Value::as_array)
                .is_some_and(|keys| keys.iter().any(|k| k.as_str() == Some(key)))
        })
    }

    /// The first dependency (`dependentRequired`, `dependentSchemas`, or draft-7
    /// `dependencies`) not yet decided for this shape.
    fn next_dependency(&mut self, shape: &Shape<'a>) -> Option<(usize, &'a str, Dependency<'a>)> {
        self.charge(shape.parts.len());
        for part in &shape.parts {
            for keyword in ["dependentRequired", "dependentSchemas", "dependencies"] {
                let Some(Value::Object(entries)) = part.get(keyword) else {
                    continue;
                };
                for (key, value) in entries {
                    let id = address(value);
                    if shape.applied.contains(&id) || shape.absent.contains(&key.as_str()) {
                        continue;
                    }
                    let dependency = match (keyword, value) {
                        ("dependentRequired" | "dependencies", Value::Array(keys)) => {
                            Dependency::Required(keys)
                        }
                        (
                            "dependentSchemas" | "dependencies",
                            schema @ (Value::Object(_) | Value::Bool(_)),
                        ) => Dependency::Schema(schema),
                        _ => continue,
                    };
                    return Some((id, key, dependency));
                }
            }
        }
        None
    }

    fn apply_dependency(&mut self, shape: &mut Shape<'a>, id: usize, dependency: Dependency<'a>) {
        shape.applied.insert(id);
        match dependency {
            Dependency::Required(keys) => {
                shape
                    .extra_required
                    .extend(keys.iter().filter_map(Value::as_str));
            }
            Dependency::Schema(schema) => self.flatten(shape, schema),
        }
    }

    /// Spend work proportional to `units` (at least one unit).
    fn charge(&mut self, units: usize) {
        let cost = units / 32 + 1;
        if self.work.saturating_add(cost) > self.max_work {
            self.work = self.max_work;
            self.exhausted = true;
        } else {
            self.work += cost;
        }
    }

    /// The kinds a shape still admits: every part's `type`, less what the branch
    /// excludes. An untyped top level is an object (tool arguments always are).
    fn shape_kinds(&mut self, shape: &Shape<'a>, depth: usize) -> Kinds {
        self.charge(shape.parts.len());
        let mut kinds = shape
            .parts
            .iter()
            .fold(K_ALL, |kinds, part| kinds & part_kinds(part));
        if kinds == K_ALL && depth == 0 {
            kinds = K_OBJ;
        }
        kinds & !shape.exclude
    }

    /// Whether `cond` holds for every value of `shape`, for none, or neither is known.
    fn settled(&mut self, cond: &'a Value, shape: &Shape<'a>, depth: usize) -> Option<bool> {
        let object = match cond {
            Value::Bool(value) => return Some(*value),
            Value::Object(object) => object,
            _ => return None,
        };
        let mut holds = true;
        for (key, value) in object {
            match key.as_str() {
                key if is_annotation(key) && key != "$ref" => {}
                "type" => {
                    let kinds = self.shape_kinds(shape, depth);
                    let allowed = part_kinds(object);
                    if kinds & allowed == 0 {
                        return Some(false);
                    }
                    holds &= kinds & !allowed == 0;
                }
                "required" => {
                    // Vacuous for non-objects: it fails only an object missing a key.
                    let keys = value.as_array()?;
                    if self.shape_kinds(shape, depth) & K_OBJ == 0 {
                        continue;
                    }
                    for key in keys.iter().filter_map(Value::as_str) {
                        if shape.absent.contains(&key) {
                            if self.shape_kinds(shape, depth) & !K_OBJ == 0 {
                                return Some(false);
                            }
                            holds = false;
                            continue;
                        }
                        holds &= self.certainly_present(shape, key);
                    }
                }
                _ => return None,
            }
        }
        holds.then_some(true)
    }

    fn pattern(&mut self, pattern: &'a str) -> Option<&regex::Regex> {
        self.regex
            .entry(pattern)
            .or_insert_with(|| {
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .ok()
            })
            .as_ref()
    }

    /// What one part says about `key`: the schemas that apply to it. `None` when
    /// the part does not constrain it, or when a pattern cannot be checked here.
    fn key_constraint(
        &mut self,
        part: &'a Map<String, Value>,
        key: &str,
    ) -> Option<Vec<&'a Value>> {
        if let Some(schema) = part
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get(key))
        {
            return Some(vec![schema]);
        }
        let mut applies = Vec::new();
        if let Some(Value::Object(patterns)) = part.get("patternProperties") {
            for (pattern, schema) in patterns {
                match self.pattern(pattern) {
                    Some(regex) if regex.is_match(key) => applies.push(schema),
                    Some(_) => {}
                    // Unknown whether it applies: leave the key unconstrained (wider).
                    None => return None,
                }
            }
        }
        if applies.is_empty() {
            if let Some(additional) = part.get("additionalProperties") {
                applies.push(additional);
            }
        }
        (!applies.is_empty()).then_some(applies)
    }

    /// What one part allows for keys it does not name.
    fn part_index(&self, part: &'a Map<String, Value>) -> Option<Option<Vec<&'a Value>>> {
        let mut types: Vec<&'a Value> = part
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(|patterns| patterns.values().collect())
            .unwrap_or_default();
        match part.get("additionalProperties") {
            Some(Value::Bool(false)) if types.is_empty() => return Some(None),
            Some(Value::Bool(false)) => {}
            Some(schema @ Value::Object(_)) => types.push(schema),
            // Absent or `true`: other keys are unconstrained.
            _ => return None,
        }
        Some(Some(types))
    }

    /// Render the conjunction of `subs`.
    fn build(&mut self, subs: &[&'a Value], depth: usize) -> Ty<'a> {
        if subs.is_empty() {
            return Ty::Unknown;
        }
        if !self.tick() {
            return Ty::Unknown;
        }
        if let [single] = subs {
            match self.plain_ref(single) {
                Some(Ok((target, reference))) => return self.reference(target, reference, depth),
                Some(Err(())) => return Ty::Unknown,
                None => {}
            }
        }
        if depth >= INLINE_DEPTH {
            return self.defer(subs);
        }
        let shape = self.shape_of(subs);
        self.render_shape(shape, depth)
    }

    /// A declaration name: `$` plus the definition's own name made an identifier.
    /// The prefix keeps every generated name clear of reserved and contextual words
    /// (`class`, `type`, `string`) and of built-in names such as `Record`.
    fn entry_name(&mut self, hint: &str) -> String {
        let base: String = std::iter::once('$')
            .chain(hint.chars().map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                    c
                } else {
                    '_'
                }
            }))
            .collect();
        let mut name = base.clone();
        let mut n = 2;
        while self.names.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        self.names.insert(name.clone());
        name
    }

    fn reference(&mut self, target: &'a Value, reference: &str, depth: usize) -> Ty<'a> {
        // At the top a definition is read as tool arguments (always an object);
        // below it an untyped definition admits other kinds too. Keep those apart.
        let top = depth == 0;
        let split = top && target.get("type").is_none();
        let key = vec![address(target), usize::from(split)];
        if let Some(&id) = self.by_key.get(&key) {
            let entry = &mut self.entries[id];
            entry.uses += 1;
            if entry.building {
                entry.recursive = true;
            }
            return Ty::Ref(id);
        }
        let hint = reference
            .rsplit('/')
            .next()
            .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
            .filter(|segment| !segment.is_empty() && segment != "#")
            .unwrap_or_else(|| "Root".to_string());
        let name = self.entry_name(&hint);
        let id = self.entries.len();
        self.entries.push(Entry {
            name,
            root: self.root,
            subs: vec![target],
            ty: None,
            building: true,
            recursive: false,
            named: false,
            uses: 1,
        });
        self.by_key.insert(key, id);
        if depth >= INLINE_DEPTH {
            self.entries[id].building = false;
            self.entries[id].named = true;
            self.pending.push(id);
        } else {
            let ty = self.build(&[target], if top { 0 } else { depth + 1 });
            let entry = &mut self.entries[id];
            entry.ty = Some(ty);
            entry.building = false;
        }
        Ty::Ref(id)
    }

    /// A subschema past [`INLINE_DEPTH`]: render it later as a named declaration.
    fn defer(&mut self, subs: &[&'a Value]) -> Ty<'a> {
        let mut key: Vec<usize> = subs.iter().map(|sub| address(sub)).collect();
        key.push(2);
        if let Some(&id) = self.by_key.get(&key) {
            self.entries[id].uses += 1;
            return Ty::Ref(id);
        }
        let name = self.entry_name("T");
        let id = self.entries.len();
        self.entries.push(Entry {
            name,
            root: self.root,
            subs: subs.to_vec(),
            ty: None,
            building: false,
            recursive: false,
            named: true,
            uses: 1,
        });
        self.by_key.insert(key, id);
        self.pending.push(id);
        Ty::Ref(id)
    }

    /// A definition that reaches itself through unions and references alone (no
    /// object, array or tuple in between) has no finite meaning TypeScript can
    /// declare. It becomes `unknown`, which is wider.
    fn settle_cycles(&mut self) {
        let mut cyclic = Vec::new();
        for id in 0..self.entries.len() {
            let mut stack: Vec<&Ty<'a>> = self.entries[id].ty.iter().collect();
            let mut seen = HashSet::new();
            let mut steps = 0usize;
            while let Some(ty) = stack.pop() {
                steps += 1;
                if steps > 100_000 {
                    cyclic.push(id);
                    break;
                }
                match ty {
                    Ty::Ref(next) if *next == id => {
                        cyclic.push(id);
                        break;
                    }
                    Ty::Ref(next) => {
                        if seen.insert(*next) {
                            stack.extend(self.entries[*next].ty.iter());
                        }
                    }
                    Ty::Union(members) | Ty::Wide(members) => stack.extend(members.iter()),
                    _ => {}
                }
            }
        }
        for id in cyclic {
            let entry = &mut self.entries[id];
            entry.ty = Some(Ty::Unknown);
            entry.recursive = false;
        }
    }

    /// Build every deferred declaration, iteratively.
    fn drain(&mut self) {
        while let Some(id) = self.pending.pop() {
            let (root, subs) = {
                let entry = &self.entries[id];
                (entry.root, entry.subs.clone())
            };
            let saved = std::mem::replace(&mut self.root, root);
            self.entries[id].building = true;
            let ty = self.build_fresh(&subs);
            self.root = saved;
            let entry = &mut self.entries[id];
            entry.ty = Some(ty);
            entry.building = false;
        }
    }

    /// [`Ctx::build`] at depth zero that never short-circuits back into the same entry.
    fn build_fresh(&mut self, subs: &[&'a Value]) -> Ty<'a> {
        if !self.tick() {
            return Ty::Unknown;
        }
        if let [single] = subs {
            match self.plain_ref(single) {
                Some(Ok((target, reference))) => return self.reference(target, reference, 0),
                Some(Err(())) => return Ty::Unknown,
                None => {}
            }
        }
        let shape = self.shape_of(subs);
        self.render_shape(shape, 0)
    }

    /// Distribute the choices (`anyOf`/`oneOf`, `if`/`then`/`else`, and
    /// dependencies whose trigger may be absent) over the merged parts, then render
    /// each resulting shape. Iterative: a work list of shapes, not recursion.
    /// Dependencies are expanded here, before rendering, because a dependency can
    /// itself bring in new choices.
    fn render_shape(&mut self, shape: Shape<'a>, depth: usize) -> Ty<'a> {
        let mut open = std::collections::VecDeque::from([shape]);
        let mut done: Vec<Shape<'a>> = Vec::new();
        while let Some(mut shape) = open.pop_front() {
            if shape.never {
                continue;
            }
            if !shape.groups.is_empty() {
                if self.exhausted {
                    return Ty::Unknown;
                }
                let group = shape.groups.remove(0);
                let mut refine_else: Option<Refinement<'a>> = None;
                let branches: Vec<Vec<&'a Value>> = match group {
                    Group::Any(members, _) => members.iter().map(|member| vec![member]).collect(),
                    Group::Cond {
                        cond,
                        then,
                        otherwise,
                    } => {
                        // The then-branch also satisfies the condition, so merge it in;
                        // the else-branch cannot say "not the condition" (wider).
                        let then_branch: Vec<&'a Value> =
                            std::iter::once(cond).chain(then).collect();
                        let else_branch: Vec<&'a Value> = otherwise.into_iter().collect();
                        refine_else = failing(cond);
                        match self.settled(cond, &shape, depth) {
                            Some(true) => vec![then_branch],
                            Some(false) => vec![else_branch],
                            None => vec![then_branch, else_branch],
                        }
                    }
                };
                let last = branches.len().saturating_sub(1);
                let two = branches.len() == 2;
                for (index, branch) in branches.into_iter().enumerate() {
                    self.charge(shape.parts.len());
                    if !self.tick() {
                        return Ty::Unknown;
                    }
                    let mut next = shape.clone();
                    for sub in branch {
                        self.flatten(&mut next, sub);
                    }
                    // The else-branch fails the condition; say what that implies. Both
                    // refinements are about object keys, which every non-object passes,
                    // so only objects reach that else-branch.
                    if two && index == last {
                        match refine_else {
                            Some(Refinement::Present(key)) => {
                                next.extra_required.push(key);
                                next.exclude |= K_ALL & !K_OBJ;
                            }
                            Some(Refinement::Absent(key)) => {
                                next.absent.push(key);
                                next.exclude |= K_ALL & !K_OBJ;
                            }
                            None => {}
                        }
                    }
                    open.push_back(next);
                }
                continue;
            }
            match self.next_dependency(&shape) {
                None => done.push(shape),
                // Out of budget with a dependency undecided: claim nothing.
                Some(_) if self.exhausted => return Ty::Unknown,
                Some((id, key, dependency)) => {
                    // Dependencies constrain only objects.
                    let kinds = self.shape_kinds(&shape, depth);
                    if kinds & K_OBJ == 0 {
                        shape.applied.insert(id);
                        open.push_front(shape);
                    } else if self.certainly_present(&shape, key) {
                        let plain =
                            kinds & !K_OBJ == 0 || matches!(dependency, Dependency::Required(_));
                        if plain {
                            self.apply_dependency(&mut shape, id, dependency);
                            open.push_front(shape);
                        } else {
                            // Objects have the trigger and take the dependency; every
                            // other kind passes it untouched.
                            self.charge(shape.parts.len());
                            let mut object = shape.clone();
                            object.exclude |= K_ALL & !K_OBJ;
                            self.apply_dependency(&mut object, id, dependency);
                            shape.applied.insert(id);
                            shape.exclude |= K_OBJ;
                            open.push_back(shape);
                            open.push_back(object);
                        }
                    } else {
                        // Optional trigger: either absent (or not an object at all),
                        // or an object with the trigger and its dependency.
                        self.charge(shape.parts.len());
                        let mut present = shape.clone();
                        present.extra_required.push(key);
                        present.exclude |= K_ALL & !K_OBJ;
                        self.apply_dependency(&mut present, id, dependency);
                        shape.absent.push(key);
                        open.push_back(shape);
                        open.push_back(present);
                    }
                }
            }
        }
        let mut members = Vec::with_capacity(done.len());
        for shape in done {
            members.push(self.render_merged(shape, depth));
        }
        union(members)
    }

    /// Render one merged shape with no groups left.
    fn render_merged(&mut self, shape: Shape<'a>, depth: usize) -> Ty<'a> {
        let required = self.required_keys(&shape);
        if shape.never {
            return Ty::Never;
        }
        // A key both required and taken as absent rules out objects only.
        let mut exclude = shape.exclude;
        if shape.absent.iter().any(|key| required.contains(key)) {
            exclude |= K_OBJ;
        }
        let mut kinds = shape
            .parts
            .iter()
            .fold(K_ALL, |kinds, part| kinds & part_kinds(part));
        let mut literals: Option<Vec<&'a Value>> = None;
        for part in &shape.parts {
            let values: Vec<&'a Value> = match (part.get("const"), part.get("enum")) {
                (Some(value), _) => vec![value],
                (None, Some(Value::Array(values))) => {
                    let mut values: Vec<&'a Value> = values.iter().collect();
                    if part.get("nullable") == Some(&Value::Bool(true)) {
                        values.push(&NULL);
                    }
                    values
                }
                _ => continue,
            };
            self.charge(values.len());
            literals = Some(match literals {
                None => values,
                Some(previous) => {
                    let keep: HashSet<String> =
                        values.iter().map(|value| value.to_string()).collect();
                    previous
                        .into_iter()
                        .filter(|value| keep.contains(&value.to_string()))
                        .collect()
                }
            });
        }
        if let Some(literals) = literals {
            // Keep the values the shape's other constraints can still accept: the
            // kind, an object's required and absent keys, an array's length bounds.
            let mut array_shape: Option<Ty<'a>> = None;
            let mut members = Vec::new();
            for value in literals {
                if kind_of(value) & kinds & !exclude == 0 {
                    continue;
                }
                let fits = match value {
                    Value::Object(map) => {
                        required.iter().all(|key| map.contains_key(*key))
                            && !shape.absent.iter().any(|key| map.contains_key(*key))
                    }
                    Value::Array(items) => {
                        let rendered = match &array_shape {
                            Some(rendered) => rendered,
                            None => array_shape.insert(self.render_array(&shape, depth)),
                        };
                        match rendered {
                            Ty::Never => false,
                            Ty::Tuple {
                                items: positions,
                                required,
                                rest,
                            } => {
                                items.len() >= *required
                                    && (rest.is_some() || items.len() <= positions.len())
                            }
                            _ => true,
                        }
                    }
                    _ => true,
                };
                if fits {
                    members.push(Ty::Lit(value));
                }
            }
            return union(members);
        }
        let mut untyped_nested = false;
        if kinds == K_ALL {
            let object = shape.parts.iter().any(|part| has_object_keywords(part));
            let array = shape.parts.iter().any(|part| has_array_keywords(part));
            if !object && !array && exclude == 0 {
                return Ty::Unknown;
            }
            if depth > 0 && exclude != 0 {
                // Untyped, with some kinds ruled out: every remaining kind, below.
                untyped_nested = true;
            } else if depth > 0 {
                // No `type` below the top: object or array keywords constrain only
                // values of that kind, and every other kind is admitted. Keep the
                // shape visible; shortening may still say `unknown`.
                let mut members = Vec::new();
                if object {
                    members.push(self.render_object(&shape, &required, depth));
                }
                members.push(self.render_array(&shape, depth));
                members.extend(["string", "number", "boolean", "null"].map(Ty::Prim));
                if !object {
                    members.push(Ty::Object {
                        props: Vec::new(),
                        index: Index::Free,
                    });
                }
                return match union(members) {
                    Ty::Union(members) => Ty::Wide(members),
                    other => other,
                };
            } else {
                // The top level: MCP tool arguments are always an object.
                kinds = if object || !array { K_OBJ } else { K_ARR };
            }
        }
        kinds &= !exclude;
        let mut members = Vec::new();
        for (bit, prim) in [(K_STR, "string"), (K_NUM, "number"), (K_BOOL, "boolean")] {
            if kinds & bit != 0 {
                members.push(Ty::Prim(prim));
            }
        }
        if kinds & K_OBJ != 0 {
            members.push(self.render_object(&shape, &required, depth));
        }
        if kinds & K_ARR != 0 {
            members.push(self.render_array(&shape, depth));
        }
        if kinds & K_NULL != 0 {
            members.push(Ty::Prim("null"));
        }
        match union(members) {
            Ty::Union(members) if untyped_nested => Ty::Wide(members),
            other => other,
        }
    }

    fn render_object(
        &mut self,
        shape: &Shape<'a>,
        required: &BTreeSet<&'a str>,
        depth: usize,
    ) -> Ty<'a> {
        // Too many declared keys for the work left: keep every required key (its
        // type `unknown`) and admit any other key. Wider, and never hides a
        // requirement; `describe` has the budget for the whole object.
        let declared_total: usize = shape
            .parts
            .iter()
            .filter_map(|part| part.get("properties").and_then(Value::as_object))
            .map(Map::len)
            .sum();
        self.charge(shape.parts.len());
        if declared_total.saturating_mul(3) > self.max_work.saturating_sub(self.work) {
            self.exhausted = true;
            let props = required
                .iter()
                .map(|key| Prop {
                    key,
                    required: true,
                    ty: Ty::Unknown,
                })
                .chain(shape.absent.iter().map(|key| Prop {
                    key,
                    required: false,
                    ty: Ty::Never,
                }))
                .collect();
            return Ty::Object {
                props,
                index: Index::Free,
            };
        }
        // Index declared properties once, so each key costs its own constraints,
        // not a scan of every part. Only parts that type undeclared keys
        // (patterns, `additionalProperties`) are consulted per key.
        let mut declared: HashMap<&'a str, Vec<&'a Value>> = HashMap::new();
        let mut generic: Vec<&'a Map<String, Value>> = Vec::new();
        let mut scanned = shape.parts.len();
        for part in &shape.parts {
            if let Some(Value::Object(properties)) = part.get("properties") {
                scanned += properties.len();
                for (key, schema) in properties {
                    declared.entry(key.as_str()).or_default().push(schema);
                }
            }
            if part.contains_key("patternProperties") || part.contains_key("additionalProperties") {
                generic.push(part);
            }
        }
        self.charge(scanned);
        let mut keys: BTreeSet<&'a str> = required.clone();
        keys.extend(declared.keys().copied());
        keys.extend(shape.absent.iter().copied());
        let mut props = Vec::with_capacity(keys.len());
        for key in keys {
            if shape.absent.contains(&key) {
                props.push(Prop {
                    key,
                    required: false,
                    ty: Ty::Never,
                });
                continue;
            }
            let mut subs = declared.get(key).cloned().unwrap_or_default();
            if !generic.is_empty() {
                self.charge(generic.len());
                for part in &generic {
                    let declares = part
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|properties| properties.contains_key(key));
                    if !declares {
                        if let Some(applies) = self.key_constraint(part, key) {
                            subs.extend(applies);
                        }
                    }
                }
            }
            let ty = if subs.is_empty() || self.exhausted {
                Ty::Unknown
            } else {
                self.build(&subs, depth + 1)
            };
            props.push(Prop {
                key,
                required: required.contains(key),
                ty,
            });
        }
        let mut constrained: Vec<Vec<&'a Value>> = Vec::new();
        let mut closed = false;
        for part in &shape.parts {
            match self.part_index(part) {
                None => {}
                Some(None) => closed = true,
                Some(Some(types)) => constrained.push(types),
            }
        }
        let index = if closed {
            Index::Closed
        } else {
            match constrained.as_slice() {
                [] => Index::Free,
                [types] => {
                    let types = types.clone();
                    let members = types
                        .into_iter()
                        .map(|schema| self.build(&[schema], depth + 1))
                        .collect();
                    match union(members) {
                        Ty::Unknown => Index::Free,
                        Ty::Never => Index::Closed,
                        ty => Index::Typed(Box::new(ty)),
                    }
                }
                // Several parts each type the extra keys; their intersection is
                // rarely worth spelling, and `unknown` is wider.
                _ => Index::Free,
            }
        };
        // A required key no value can fill: no object matches.
        if props
            .iter()
            .any(|prop| prop.required && matches!(prop.ty, Ty::Never))
        {
            return Ty::Never;
        }
        Ty::Object { props, index }
    }

    fn render_array(&mut self, shape: &Shape<'a>, depth: usize) -> Ty<'a> {
        // Per part: the positional schemas and what applies after them.
        let mut prefixes: Vec<(&'a [Value], Option<&'a Value>)> = Vec::new();
        let mut min_items = 0u64;
        let mut max_items: Option<u64> = None;
        for part in &shape.parts {
            let prefix: &'a [Value] = match (part.get("prefixItems"), part.get("items")) {
                (Some(Value::Array(prefix)), _) => prefix,
                // Draft 2019 and earlier spell a tuple as an `items` array.
                (None, Some(Value::Array(prefix))) => prefix,
                _ => &[],
            };
            let rest = match (part.get("prefixItems"), part.get("items")) {
                (None, Some(Value::Array(_))) => part.get("additionalItems"),
                (_, Some(items @ (Value::Object(_) | Value::Bool(_)))) => Some(items),
                _ => None,
            };
            prefixes.push((prefix, rest));
            if let Some(min) = part.get("minItems").and_then(Value::as_u64) {
                min_items = min_items.max(min);
            }
            if let Some(max) = part.get("maxItems").and_then(Value::as_u64) {
                max_items = Some(max_items.map_or(max, |previous| previous.min(max)));
            }
        }
        let positions = prefixes
            .iter()
            .map(|(prefix, _)| prefix.len())
            .max()
            .unwrap_or(0);
        let mut items = Vec::new();
        let limit = max_items.map_or(positions, |max| positions.min(max as usize));
        for index in 0..limit {
            let subs: Vec<&'a Value> = prefixes
                .iter()
                .filter_map(|(prefix, rest)| prefix.get(index).or(*rest))
                .collect();
            items.push(self.build(&subs, depth + 1));
        }
        let rest_subs: Vec<&'a Value> = prefixes.iter().filter_map(|(_, rest)| *rest).collect();
        let mut rest = if max_items.is_some_and(|max| max as usize <= positions) {
            Ty::Never
        } else if rest_subs.is_empty() {
            Ty::Unknown
        } else {
            self.build(&rest_subs, depth + 1)
        };
        // Spell out positions a length bound requires or limits, when that is short.
        let len = items.len() as u64;
        if !matches!(rest, Ty::Never) {
            if min_items > len && min_items - len <= MAX_SPELLED_POSITIONS {
                for _ in len..min_items {
                    items.push(rest.clone());
                }
            }
            // A plain array keeps `T[]` under `maxItems`: wider, and easier to read.
            if let Some(max) = max_items.filter(|_| positions > 0) {
                let len = items.len() as u64;
                if max >= len && max - len <= MAX_SPELLED_POSITIONS {
                    for _ in len..max {
                        items.push(rest.clone());
                    }
                    rest = Ty::Never;
                }
            }
        }
        let closed = matches!(rest, Ty::Never);
        // No array has more items than a closed tuple allows, or more than maxItems.
        if (closed && min_items > items.len() as u64)
            || max_items.is_some_and(|max| max < min_items)
        {
            return Ty::Never;
        }
        let required = (min_items as usize).min(items.len());
        let rest = (!closed).then(|| Box::new(rest));
        match (items.is_empty(), rest) {
            (true, Some(rest)) => Ty::Array(rest),
            (_, rest) => Ty::Tuple {
                items,
                required,
                rest,
            },
        }
    }

    /// Whether the conjunction of `subs` accepts the empty object `{}`, which is
    /// what a stub sends when `args` is left out.
    fn accepts_empty(&mut self, sub: &'a Value) -> Tri {
        let key = address(sub);
        if let Some(&known) = self.empty_memo.get(&key) {
            return known;
        }
        // In progress: a cycle cannot be settled this way.
        self.empty_memo.insert(key, Tri::Maybe);
        let answer = self.accepts_empty_uncached(sub);
        self.empty_memo.insert(key, answer);
        answer
    }

    /// Evaluates `{}` against the schema: every keyword that can reject an empty
    /// object is checked, `not` and `if`/`then`/`else` by evaluating their
    /// subschemas, and only what cannot be seen (an external `$ref`) is `Maybe`.
    fn accepts_empty_uncached(&mut self, sub: &'a Value) -> Tri {
        let shape = self.shape_of(&[sub]);
        if shape.never {
            return Tri::No;
        }
        let mut answer = if shape.opaque || self.exhausted {
            Tri::Maybe
        } else {
            Tri::Yes
        };
        let empty = Value::Object(Map::new());
        let and = |answer: &mut Tri, part: Tri| match part {
            Tri::No => false,
            Tri::Maybe => {
                *answer = Tri::Maybe;
                true
            }
            Tri::Yes => true,
        };
        for part in &shape.parts {
            if part_kinds(part) & K_OBJ == 0 {
                return Tri::No;
            }
            if let Some(value) = part.get("const") {
                if value != &empty {
                    return Tri::No;
                }
            }
            if let Some(Value::Array(values)) = part.get("enum") {
                if !values.contains(&empty) {
                    return Tri::No;
                }
            }
            if part
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|keys| !keys.is_empty())
            {
                return Tri::No;
            }
            if part
                .get("minProperties")
                .and_then(Value::as_u64)
                .is_some_and(|min| min > 0)
            {
                return Tri::No;
            }
            if let Some(not) = part.get("not") {
                let negated = match self.accepts_empty(not) {
                    Tri::Yes => Tri::No,
                    Tri::No => Tri::Yes,
                    Tri::Maybe => Tri::Maybe,
                };
                if !and(&mut answer, negated) {
                    return Tri::No;
                }
            }
        }
        // `properties`, patterns, extra keys, dependencies and property names are
        // vacuous for an object with no keys.
        for group in shape.groups.clone() {
            let result = match group {
                Group::Any(members, one_of) => {
                    let results: Vec<Tri> = members
                        .iter()
                        .map(|member| self.accepts_empty(member))
                        .collect();
                    let yes = results.iter().filter(|r| **r == Tri::Yes).count();
                    let maybe = results.iter().filter(|r| **r == Tri::Maybe).count();
                    if one_of {
                        match (yes, maybe) {
                            (1, 0) => Tri::Yes,
                            (0, 0) => Tri::No,
                            (yes, _) if yes >= 2 => Tri::No,
                            _ => Tri::Maybe,
                        }
                    } else if yes > 0 {
                        Tri::Yes
                    } else if maybe > 0 {
                        Tri::Maybe
                    } else {
                        Tri::No
                    }
                }
                Group::Cond {
                    cond,
                    then,
                    otherwise,
                } => {
                    let then = then.map_or(Tri::Yes, |then| self.accepts_empty(then));
                    let otherwise = otherwise.map_or(Tri::Yes, |other| self.accepts_empty(other));
                    match self.accepts_empty(cond) {
                        Tri::Yes => then,
                        Tri::No => otherwise,
                        Tri::Maybe if then == otherwise => then,
                        Tri::Maybe => Tri::Maybe,
                    }
                }
            };
            if !and(&mut answer, result) {
                return Tri::No;
            }
        }
        answer
    }
}

/// How one print pass renders a type.
#[derive(Clone, Copy)]
struct Step {
    /// Render every object below the top level as `object`.
    collapse: bool,
    typed_output: bool,
    fold_optional: bool,
    /// Render literal unions as their base types.
    enum_base: bool,
    /// Keep required names that do not fit as `name: unknown`, then fold the rest.
    trim_required: bool,
}

impl Step {
    const FULL: Step = Step {
        collapse: false,
        typed_output: true,
        fold_optional: false,
        enum_base: false,
        trim_required: false,
    };
}

const STEPS: [Step; 6] = [
    Step::FULL,
    Step {
        collapse: true,
        ..Step::FULL
    },
    Step {
        collapse: true,
        typed_output: false,
        ..Step::FULL
    },
    Step {
        collapse: true,
        typed_output: false,
        fold_optional: true,
        ..Step::FULL
    },
    Step {
        collapse: true,
        typed_output: false,
        fold_optional: true,
        enum_base: true,
        trim_required: false,
    },
    Step {
        collapse: true,
        typed_output: false,
        fold_optional: true,
        enum_base: true,
        trim_required: true,
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A search hit: recursive and deferred definitions are named, everything
    /// else inlined, and a note points at `toolport.describe` for the names.
    Search,
    /// `describe`: recursive and deferred definitions become declarations; with
    /// `hoist_shared`, every definition used more than once does too.
    Describe { hoist_shared: bool },
}

struct Printer<'c, 'a> {
    ctx: &'c Ctx<'a>,
    step: Step,
    mode: Mode,
    budget: usize,
    out: String,
    chars: usize,
    /// Definitions printed by name, in first-use order.
    named: Vec<usize>,
    inlining: Vec<usize>,
}

impl<'c, 'a> Printer<'c, 'a> {
    fn new(ctx: &'c Ctx<'a>, step: Step, mode: Mode, budget: usize) -> Self {
        Printer {
            ctx,
            step,
            mode,
            budget,
            out: String::new(),
            chars: 0,
            named: Vec::new(),
            inlining: Vec::new(),
        }
    }

    fn push(&mut self, text: &str) -> Result<(), TooLong> {
        self.chars += text.chars().count();
        if self.chars > self.budget {
            return Err(TooLong);
        }
        self.out.push_str(text);
        Ok(())
    }

    fn mark(&self) -> (usize, usize, usize) {
        (self.out.len(), self.chars, self.named.len())
    }

    fn reset(&mut self, mark: (usize, usize, usize)) {
        self.out.truncate(mark.0);
        self.chars = mark.1;
        self.named.truncate(mark.2);
    }

    fn compound(&self, ty: &Ty<'a>) -> bool {
        let mut ty = ty;
        let mut seen: Vec<usize> = Vec::new();
        // Follow inlined references to what they print as, without looping.
        while let Ty::Ref(id) = ty {
            if self.by_name(*id, 1) || seen.contains(id) {
                return false;
            }
            seen.push(*id);
            match self.ctx.entries[*id].ty.as_ref() {
                Some(next) => ty = next,
                None => return false,
            }
        }
        match ty {
            Ty::Wide(_) => !self.step.collapse,
            Ty::Union(members) => {
                if self.step.enum_base && members.iter().all(|m| matches!(m, Ty::Lit(_))) {
                    let kinds: HashSet<Kinds> = members
                        .iter()
                        .map(|m| match m {
                            Ty::Lit(v) => kind_of(v),
                            _ => 0,
                        })
                        .collect();
                    kinds.len() > 1
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    /// Whether a reference prints as its name at `depth`.
    fn by_name(&self, id: usize, depth: usize) -> bool {
        let entry = &self.ctx.entries[id];
        if entry.named
            || entry.ty.is_none()
            || self.inlining.contains(&id)
            || depth > INLINE_DEPTH * 2
        {
            return true;
        }
        match self.mode {
            Mode::Search => false,
            Mode::Describe { hoist_shared } => entry.recursive || (hoist_shared && entry.uses > 1),
        }
    }

    fn print(&mut self, ty: &Ty<'a>, depth: usize) -> Result<(), TooLong> {
        match ty {
            Ty::Unknown => self.push("unknown"),
            Ty::Never => self.push("never"),
            Ty::Prim(prim) => self.push(prim),
            Ty::Lit(value) => {
                if self.step.enum_base {
                    self.push(base_type(value))
                } else {
                    self.literal(value)
                }
            }
            Ty::Array(item) => {
                self.element(item, depth)?;
                self.push("[]")
            }
            Ty::Tuple {
                items,
                required,
                rest,
            } => {
                self.push("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.push(", ")?;
                    }
                    if index < *required {
                        self.print(item, depth + 1)?;
                    } else {
                        self.element(item, depth)?;
                        self.push("?")?;
                    }
                }
                if let Some(rest) = rest {
                    if !items.is_empty() {
                        self.push(", ")?;
                    }
                    self.push("...")?;
                    self.element(rest, depth)?;
                    self.push("[]")?;
                }
                self.push("]")
            }
            Ty::Object { props, index } => self.object(props, index, depth),
            Ty::Union(members) => self.union(members, depth),
            Ty::Wide(_) if self.step.collapse => self.push("unknown"),
            Ty::Wide(members) => self.union(members, depth),
            Ty::Ref(id) => {
                let id = *id;
                if self.by_name(id, depth) {
                    if !self.named.contains(&id) {
                        self.named.push(id);
                    }
                    let name = self.ctx.entries[id].name.clone();
                    return self.push(&name);
                }
                let ctx = self.ctx;
                let ty = ctx.entries[id].ty.as_ref().expect("built");
                self.inlining.push(id);
                let printed = self.print(ty, depth);
                self.inlining.pop();
                printed
            }
        }
    }

    /// An array element or optional tuple position, parenthesised when compound.
    fn element(&mut self, ty: &Ty<'a>, depth: usize) -> Result<(), TooLong> {
        if self.compound(ty) {
            self.push("(")?;
            self.print(ty, depth + 1)?;
            self.push(")")
        } else {
            self.print(ty, depth + 1)
        }
    }

    fn union(&mut self, members: &[Ty<'a>], depth: usize) -> Result<(), TooLong> {
        let mut seen: Vec<String> = Vec::new();
        for member in members {
            let start = self.mark();
            if !seen.is_empty() {
                self.push(" | ")?;
            }
            let text_start = self.out.len();
            self.print(member, depth)?;
            let text = self.out[text_start..].to_string();
            // Literal unions under `enum_base` repeat their base type; say it once.
            if seen.len() <= 64 && seen.contains(&text) {
                self.reset(start);
            } else if seen.len() <= 64 {
                seen.push(text);
            } else {
                seen.push(String::new());
            }
        }
        Ok(())
    }

    fn literal(&mut self, value: &Value) -> Result<(), TooLong> {
        match value {
            Value::Array(items) => {
                self.push("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.push(", ")?;
                    }
                    self.literal(item)?;
                }
                self.push("]")
            }
            Value::Object(map) => {
                self.push("{")?;
                for (index, (key, item)) in map.iter().enumerate() {
                    if index > 0 {
                        self.push("; ")?;
                    }
                    self.push(&property_key(key))?;
                    self.push(": ")?;
                    self.literal(item)?;
                }
                self.push("}")
            }
            Value::String(text) => self.push(&quote(text)),
            scalar => {
                self.push(&serde_json::to_string(scalar).unwrap_or_else(|_| "unknown".into()))
            }
        }
    }

    fn object(
        &mut self,
        props: &[Prop<'a>],
        index: &Index<'a>,
        depth: usize,
    ) -> Result<(), TooLong> {
        if self.step.collapse && depth > 0 {
            return self.push("object");
        }
        if props.is_empty() {
            return match index {
                Index::Free => self.push("Record<string, unknown>"),
                Index::Closed => self.push("{}"),
                Index::Typed(ty) => {
                    self.push("Record<string, ")?;
                    self.print(ty, depth + 1)?;
                    self.push(">")
                }
            };
        }
        let top = depth == 0;
        let fold_optional = top && self.step.fold_optional;
        let trim = top && self.step.trim_required;
        // Room kept for the fold markers when trimming.
        let reserve = if trim { 48 } else { 0 };
        self.push("{")?;
        let mut first = true;
        let (mut folded_required, mut folded_optional) = (0usize, 0usize);
        let mut prop_types: Vec<String> = Vec::new();
        let mut any_optional = false;
        for prop in props {
            if !prop.required && fold_optional {
                folded_optional += 1;
                continue;
            }
            if folded_required > 0 {
                if prop.required {
                    folded_required += 1;
                } else {
                    folded_optional += 1;
                }
                continue;
            }
            let mark = self.mark();
            let attempt = self.prop(prop, first, depth, reserve, &mut prop_types);
            match attempt {
                Ok(()) => {
                    first = false;
                    any_optional |= !prop.required;
                }
                Err(TooLong) if trim => {
                    self.reset(mark);
                    // Keep the name; `unknown` is wider than any type.
                    let wide = Prop {
                        key: prop.key,
                        required: prop.required,
                        ty: Ty::Unknown,
                    };
                    let mark = self.mark();
                    match self.prop(&wide, first, depth, reserve, &mut prop_types) {
                        Ok(()) => {
                            first = false;
                            any_optional |= !prop.required;
                        }
                        Err(TooLong) => {
                            self.reset(mark);
                            if prop.required {
                                folded_required = 1;
                            } else {
                                folded_optional += 1;
                            }
                        }
                    }
                }
                Err(TooLong) => return Err(TooLong),
            }
        }
        if let Index::Typed(ty) = index {
            if !first {
                self.push("; ")?;
            }
            first = false;
            self.push("[key: string]: ")?;
            let start = self.out.len();
            self.print(ty, depth + 1)?;
            let index_text = self.out[start..].to_string();
            // An index signature must admit every named property's type.
            let mut seen = vec![index_text];
            for text in prop_types {
                if text != "never" && !seen.contains(&text) {
                    self.push(" | ")?;
                    self.push(&text)?;
                    seen.push(text);
                }
            }
            // Under `strict`, an optional property reads as `T | undefined`.
            if any_optional && !seen.iter().any(|text| text == "unknown") {
                self.push(" | undefined")?;
            }
        }
        for (count, label) in [(folded_required, "required"), (folded_optional, "optional")] {
            if count > 0 {
                if !first {
                    self.push("; ")?;
                }
                first = false;
                self.push(&format!("...+{count} {label}"))?;
            }
        }
        self.push("}")
    }

    fn prop(
        &mut self,
        prop: &Prop<'a>,
        first: bool,
        depth: usize,
        reserve: usize,
        prop_types: &mut Vec<String>,
    ) -> Result<(), TooLong> {
        if !first {
            self.push("; ")?;
        }
        self.push(&property_key(prop.key))?;
        self.push(if prop.required { ": " } else { "?: " })?;
        let start = self.out.len();
        self.print(&prop.ty, depth + 1)?;
        if self.chars + reserve > self.budget {
            return Err(TooLong);
        }
        let text = self.out[start..].to_string();
        if !prop_types.contains(&text) {
            prop_types.push(text);
        }
        Ok(())
    }
}

/// The base type of a literal, for the `enum_base` step.
fn base_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "unknown[]",
        Value::Object(_) => "object",
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// A property name, quoted as a JSON string unless it is a plain ASCII identifier.
/// JSON escapes cover quotes, backslashes and control characters; U+2028 and U+2029
/// are escaped too so the text stays one line in any JavaScript context.
fn property_key(key: &str) -> String {
    if is_identifier(key) {
        key.to_string()
    } else {
        quote(key)
    }
}

fn quote(text: &str) -> String {
    serde_json::to_string(text)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn member_access(segment: &str) -> String {
    if is_identifier(segment) {
        format!(".{segment}")
    } else {
        format!("[{}]", quote(segment))
    }
}

/// Render `schema` as a TypeScript type of at most `max_chars` characters, with no
/// shortening. `None` means it did not fit.
pub fn render_type(schema: &Value, max_chars: usize) -> Option<String> {
    let mut ctx = Ctx::new(schema, DESCRIBE_WORK);
    let ty = ctx.build(&[schema], 0);
    ctx.drain();
    ctx.settle_cycles();
    let mut printer = Printer::new(&ctx, Step::FULL, Mode::Search, max_chars);
    printer.print(&ty, 0).ok()?;
    Some(printer.out)
}

/// The parts of one tool a signature is made from, built once and printed per step.
struct Built<'a> {
    ctx: Ctx<'a>,
    args: Ty<'a>,
    output: Option<Ty<'a>>,
    optional: bool,
}

fn build_tool<'a>(input: &'a Value, output: Option<&'a Value>, max_work: usize) -> Built<'a> {
    let mut ctx = Ctx::new(input, max_work);
    // Settled before the build spends the work budget.
    let optional = matches!(input, Value::Null) || ctx.accepts_empty(input) == Tri::Yes;
    let args = match input {
        // A tool without a schema takes any object.
        Value::Null => Ty::Object {
            props: Vec::new(),
            index: Index::Free,
        },
        input => ctx.build(&[input], 0),
    };
    let output = output.map(|output| {
        ctx.root = output;
        let ty = ctx.build(&[output], 0);
        ctx.root = input;
        ty
    });
    ctx.drain();
    ctx.settle_cycles();
    Built {
        ctx,
        args,
        output,
        optional,
    }
}

/// Print a signature for `callee` under one step and mode. `None` when over `cap`.
fn print_signature(
    built: &Built<'_>,
    callee: &str,
    step: Step,
    mode: Mode,
    cap: usize,
) -> Option<(String, Vec<usize>)> {
    let mut printer = Printer::new(&built.ctx, step, mode, cap);
    printer.push(callee).ok()?;
    printer
        .push(if built.optional { "args?: " } else { "args: " })
        .ok()?;
    printer.print(&built.args, 0).ok()?;
    printer.push("): ToolResult").ok()?;
    if let Some(output) = built.output.as_ref().filter(|_| step.typed_output) {
        if !output.is_unknown() {
            printer.push("<").ok()?;
            printer.print(output, 0).ok()?;
            printer.push(">").ok()?;
        }
    }
    Some((printer.out, printer.named))
}

/// Search mode: a named definition is referenced by name; say where it is defined.
fn with_name_note(text: String, named: &[usize], ctx: &Ctx<'_>) -> String {
    if named.is_empty() {
        return text;
    }
    let names: Vec<&str> = named
        .iter()
        .map(|id| ctx.entries[*id].name.as_str())
        .collect();
    format!("{text} /* {}: see toolport.describe */", names.join(", "))
}

/// `servers.<server>.<tool>(` for a namespaced name, else `toolport.call("<name>", `.
fn callee(name: &str) -> String {
    match name.split_once("__").or_else(|| name.split_once('-')) {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => {
            format!("servers{}{}(", member_access(server), member_access(tool))
        }
        _ => call_callee(name),
    }
}

fn call_callee(name: &str) -> String {
    format!("toolport.call({}, ", quote(name))
}

/// The callee used when the name itself does not fit: `name` is the hit's own name.
const NAMELESS_CALLEE: &str = "toolport.call(name, ";

/// The name and schemas, borrowed. Nothing is copied: every string the schema
/// contributes reaches the text through the printer, and the finished text passes
/// the same gateway-voice neutralisation a listed `inputSchema` gets.
fn tool_parts(tool: &Value) -> Option<(&str, &Value, Option<&Value>)> {
    let name = tool.get("name").and_then(Value::as_str)?;
    let input = tool.get("inputSchema").unwrap_or(&NULL);
    let output = tool.get("outputSchema").filter(|schema| schema.is_object());
    Some((name, input, output))
}

fn finish(text: String, cap: usize) -> Option<String> {
    let text = integrity::neutralize_gateway_voice(&text);
    (text.chars().count() <= cap).then_some(text)
}

fn search_signature_of(built: &Built<'_>, name: &str, cap: usize) -> String {
    let preferred = callee(name);
    let mut callees = vec![preferred.clone()];
    let call = call_callee(name);
    if call != preferred {
        callees.push(call);
    }
    callees.push(NAMELESS_CALLEE.to_string());
    for callee in &callees {
        for step in STEPS {
            let Some((text, named)) = print_signature(built, callee, step, Mode::Search, cap)
            else {
                continue;
            };
            if let Some(text) = finish(with_name_note(text, &named, &built.ctx), cap) {
                return text;
            }
        }
    }
    // Every step folds to `...+N required` at worst, so only a pathological
    // neutralisation gets here. Still say how to call it, within the cap.
    format!(
        "{NAMELESS_CALLEE}args{}: object): ToolResult",
        if built.optional { "?" } else { "" }
    )
}

/// The Code Mode signature a search hit carries, at most [`MAX_SIGNATURE_CHARS`]
/// characters, shortened in steps when needed. `None` only for an entry without a name.
///
/// `servers.<server>.<tool>` matches the stubs Code Mode installs for a namespaced
/// name, in either the `server__tool` or dashed `server-tool` spelling; any other
/// name uses `toolport.call("<name>", args)`, and a name too long for the cap uses
/// `toolport.call(name, args)` where `name` is the hit's own name. Strings from
/// `inputSchema` and `outputSchema` pass through the same neutralisation as a listed
/// `inputSchema`. The return type is `ToolResult`, typed with `structuredContent`
/// when the upstream tool declares an `outputSchema`: calls inside a script are not
/// shaped. A recursive type is named, with a note that `toolport.describe` has it.
pub fn signature(tool: &Value) -> Option<String> {
    let (name, input, output) = tool_parts(tool)?;
    let built = build_tool(input, output, SEARCH_WORK);
    Some(search_signature_of(&built, name, MAX_SIGNATURE_CHARS))
}

/// What `toolport.describe` returns for a tool's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullSignature {
    /// Named declarations (`type Node = ...;`), one per line, then the signature.
    pub signature: String,
    /// Set when a guard was reached: why the signature is not the complete one.
    pub truncated: Option<String>,
}

/// The complete signature `toolport.describe` returns: never shortened, recursive
/// and deeply nested definitions written as named declarations ahead of it. Only a
/// schema past the work or size guard is not complete, and then `truncated` says why.
pub fn full_signature(tool: &Value) -> Option<FullSignature> {
    full_signature_with(tool, DESCRIBE_WORK, MAX_DESCRIBE_SIGNATURE_BYTES)
}

fn full_signature_with(tool: &Value, max_work: usize, max_bytes: usize) -> Option<FullSignature> {
    let (name, input, output) = tool_parts(tool)?;
    let built = build_tool(input, output, max_work);
    let callee = callee(name);
    let full = |hoist_shared: bool, cap: usize| {
        let mode = Mode::Describe { hoist_shared };
        let (signature, named) = print_signature(&built, &callee, Step::FULL, mode, cap)?;
        let mut declarations = String::new();
        let mut chars = signature.chars().count();
        let mut done = 0;
        let mut named = named;
        while done < named.len() {
            let id = named[done];
            done += 1;
            let mut printer = Printer::new(&built.ctx, Step::FULL, mode, cap.checked_sub(chars)?);
            printer.inlining.push(id);
            printer.named = named.clone();
            let entry = &built.ctx.entries[id];
            printer.push("type ").ok()?;
            printer.push(&entry.name).ok()?;
            printer.push(" = ").ok()?;
            printer
                .print(entry.ty.as_ref().unwrap_or(&Ty::Unknown), 0)
                .ok()?;
            printer.push(";\n").ok()?;
            chars += printer.chars;
            declarations.push_str(&printer.out);
            named = printer.named;
        }
        Some(format!("{declarations}{signature}"))
    };
    let text = full(false, DESCRIBE_INLINE_CHARS.min(max_bytes))
        .or_else(|| full(true, max_bytes))
        .filter(|text| text.len() <= max_bytes);
    let mut truncated = built.ctx.exhausted.then(|| {
        format!("the schema exceeds the {max_work}-node work guard; types past it read unknown")
    });
    let signature = match text {
        Some(text) => integrity::neutralize_gateway_voice(&text),
        None => {
            truncated = Some(format!(
                "the full signature exceeds the {max_bytes}-byte guard; this is \
                 the shortened search form, and inputSchema is complete"
            ));
            search_signature_of(&built, name, MAX_SIGNATURE_CHARS)
        }
    };
    Some(FullSignature {
        signature,
        truncated,
    })
}

#[cfg(test)]
#[path = "tool_signature_tests.rs"]
mod tests;
