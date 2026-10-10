//! Immutable tool definitions shared by router views and gateway catalogs.
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Write,
    ops::{Deref, Index},
    sync::Arc,
};

struct DigestWriter(Sha256);
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn content_digest(value: &impl Serialize) -> [u8; 32] {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).expect("JSON values serialize");
    writer.0.finalize().into()
}

#[derive(Debug)]
pub struct ToolDefinition {
    value: Value,
    // Downstream definition before client schema compatibility changes.
    source: Option<Value>,
    pub(crate) arguments: Option<Arc<crate::schema_compat::ArgumentMap>>,
    pub digest: [u8; 32],
}
impl ToolDefinition {
    pub(crate) fn with_arguments(
        value: Value,
        source: Value,
        arguments: Option<Arc<crate::schema_compat::ArgumentMap>>,
    ) -> Self {
        Self {
            digest: content_digest(&value),
            source: (source != value).then_some(source),
            value,
            arguments,
        }
    }
    pub(crate) fn source(&self) -> &Value {
        self.source.as_ref().unwrap_or(&self.value)
    }
    pub fn new(value: Value) -> Self {
        Self {
            digest: content_digest(&value),
            value,
            source: None,
            arguments: None,
        }
    }
}
impl Deref for ToolDefinition {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}
impl Serialize for ToolDefinition {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(s)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SharedTools(pub Vec<Arc<ToolDefinition>>);
impl SharedTools {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Value> {
        self.0.iter().map(|tool| &tool.value)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.0.get(index).map(|tool| &tool.value)
    }
    pub fn to_vec(&self) -> Vec<Value> {
        self.iter().cloned().collect()
    }
    pub fn as_ptr(&self) -> *const Arc<ToolDefinition> {
        self.0.as_ptr()
    }
    pub fn sort(&mut self) {
        self.0.sort_by(|a, b| {
            a.get("name")
                .and_then(Value::as_str)
                .cmp(&b.get("name").and_then(Value::as_str))
        });
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&Value) -> bool) {
        self.0.retain(|tool| keep(tool));
    }
    pub fn push(&mut self, value: Value) {
        self.0.push(Arc::new(ToolDefinition::new(value)));
    }
}
impl Index<usize> for SharedTools {
    type Output = Value;
    fn index(&self, index: usize) -> &Value {
        &self.0[index].value
    }
}
impl From<Vec<Value>> for SharedTools {
    fn from(values: Vec<Value>) -> Self {
        Self(
            values
                .into_iter()
                .map(|v| Arc::new(ToolDefinition::new(v)))
                .collect(),
        )
    }
}
impl Serialize for SharedTools {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len()))?;
        for tool in self.iter() {
            seq.serialize_element(tool)?;
        }
        seq.end()
    }
}
impl PartialEq for SharedTools {
    fn eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .map(|t| t.digest)
            .eq(other.0.iter().map(|t| t.digest))
    }
}
impl PartialEq<Vec<Value>> for SharedTools {
    fn eq(&self, other: &Vec<Value>) -> bool {
        self.iter().eq(other.iter())
    }
}
impl IntoIterator for SharedTools {
    type Item = Value;
    type IntoIter =
        std::iter::Map<std::vec::IntoIter<Arc<ToolDefinition>>, fn(Arc<ToolDefinition>) -> Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter().map(|tool| match Arc::try_unwrap(tool) {
            Ok(definition) => definition.value,
            Err(definition) => definition.value.clone(),
        })
    }
}
impl<'a> IntoIterator for &'a SharedTools {
    type Item = &'a Value;
    type IntoIter = std::iter::Map<
        std::slice::Iter<'a, Arc<ToolDefinition>>,
        fn(&'a Arc<ToolDefinition>) -> &'a Value,
    >;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().map(|t| &t.value)
    }
}

/// Read paths accept both fixture values and shared production definitions.
pub trait ToolCatalog {
    fn values(&self) -> Box<dyn Iterator<Item = &Value> + '_>;
    fn source_values(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        self.values()
    }
    fn len(&self) -> usize;
    fn address(&self) -> usize;
    fn get(&self, index: usize) -> Option<&Value>;
    fn shared(&self) -> SharedTools;
    fn digests(&self) -> Vec<[u8; 32]> {
        self.values().map(content_digest).collect()
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn to_vec(&self) -> Vec<Value> {
        self.values().cloned().collect()
    }
}
impl dyn ToolCatalog + '_ {
    pub fn iter(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        self.values()
    }
}
impl ToolCatalog for SharedTools {
    fn source_values(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        Box::new(self.0.iter().map(|tool| tool.source()))
    }
    fn values(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        Box::new(self.iter())
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn address(&self) -> usize {
        self.as_ptr() as usize
    }
    fn get(&self, index: usize) -> Option<&Value> {
        self.get(index)
    }
    fn digests(&self) -> Vec<[u8; 32]> {
        self.0.iter().map(|tool| tool.digest).collect()
    }
    fn shared(&self) -> SharedTools {
        self.clone()
    }
}
impl ToolCatalog for Vec<Value> {
    fn values(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        Box::new(self.as_slice().iter())
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn address(&self) -> usize {
        self.as_ptr() as usize
    }
    fn get(&self, index: usize) -> Option<&Value> {
        self.as_slice().get(index)
    }
    fn shared(&self) -> SharedTools {
        self.clone().into()
    }
}
impl ToolCatalog for [Value] {
    fn values(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        Box::new(self.iter())
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn address(&self) -> usize {
        self.as_ptr() as usize
    }
    fn get(&self, index: usize) -> Option<&Value> {
        self.get(index)
    }
    fn shared(&self) -> SharedTools {
        self.to_vec().into()
    }
}
impl<const N: usize> ToolCatalog for [Value; N] {
    fn values(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        Box::new(self.as_slice().iter())
    }
    fn len(&self) -> usize {
        N
    }
    fn address(&self) -> usize {
        self.as_ptr() as usize
    }
    fn get(&self, index: usize) -> Option<&Value> {
        self.as_slice().get(index)
    }
    fn shared(&self) -> SharedTools {
        self.to_vec().into()
    }
}
pub struct CatalogRef<'a>(pub &'a dyn ToolCatalog);
impl Serialize for CatalogRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for tool in self.0.iter() {
            seq.serialize_element(tool)?;
        }
        seq.end()
    }
}

/// Original downstream catalogs stored as immutable canonical JSON bytes.
/// Name, policy, app and header metadata are indexed without a second parsed tree.
/// Materialization explicitly parses bytes each time. Router normalization caches
/// the resulting definition per slot; list/search/profile/notification paths use
/// those shared definitions instead of materializing this storage.
#[derive(Clone, Debug, Default)]
pub struct SerializedTools(Arc<SerializedCatalog>);
#[derive(Debug, Clone, Default)]
struct SerializedCatalog {
    tools: Vec<Arc<RawTool>>,
    by_name: HashMap<String, usize>,
}
impl SerializedCatalog {
    fn index_names(&mut self) {
        self.by_name.clear();
        for (index, tool) in self.tools.iter().enumerate() {
            if let Some(name) = &tool.name {
                // Match the original first-match lookup for duplicate names.
                self.by_name.entry(name.clone()).or_insert(index);
            }
        }
    }
}

/// Policy inputs compiled from the original downstream definition. Add future
/// definition-dependent policy fields here so every enforcement path supplies them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolPolicyMetadata {
    pub destructive: bool,
}
impl From<&Value> for ToolPolicyMetadata {
    fn from(tool: &Value) -> Self {
        Self {
            destructive: crate::router::is_destructive(tool),
        }
    }
}
#[derive(Debug, Clone)]
struct RawTool {
    name: Option<String>,
    policy: ToolPolicyMetadata,
    headers: Result<Vec<crate::downstream::HeaderParamSpec>, String>,
    app_uri: Option<String>,
    json: Box<serde_json::value::RawValue>,
    digest: [u8; 32],
}
impl RawTool {
    fn new(value: Value) -> Self {
        Self {
            policy: ToolPolicyMetadata::from(&value),
            headers: crate::downstream::header_param_specs(&value),
            app_uri: value
                .pointer("/_meta/ui/resourceUri")
                .or_else(|| value.pointer("/_meta/ui~1resourceUri"))
                .and_then(Value::as_str)
                .filter(|uri| uri.starts_with("ui://"))
                .map(str::to_string),
            name: value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            digest: content_digest(&value),
            json: serde_json::value::to_raw_value(&value).expect("JSON value serializes"),
        }
    }
    fn parse(&self) -> Value {
        serde_json::from_str(self.json.get()).expect("stored JSON is valid")
    }
}
impl SerializedTools {
    #[cfg(test)]
    pub(crate) fn storage_sharers(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    pub fn len(&self) -> usize {
        self.0.tools.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.tools.is_empty()
    }
    pub fn names(&self) -> Vec<Option<&str>> {
        self.0
            .tools
            .iter()
            .map(|tool| tool.name.as_deref())
            .collect()
    }
    pub fn contains_name(&self, name: &str) -> bool {
        self.0.by_name.contains_key(name)
    }
    pub(crate) fn policy_metadata(&self, index: usize) -> ToolPolicyMetadata {
        self.0.tools[index].policy
    }
    pub(crate) fn header_specs(
        &self,
        name: &str,
    ) -> Result<&[crate::downstream::HeaderParamSpec], String> {
        let Some(index) = self.0.by_name.get(name) else {
            return Ok(&[]);
        };
        self.0.tools[*index]
            .headers
            .as_deref()
            .map_err(Clone::clone)
    }
    pub fn app_uri(&self, index: usize) -> Option<&str> {
        self.0.tools[index].app_uri.as_deref()
    }
    pub fn digest(&self, index: usize) -> [u8; 32] {
        self.0.tools[index].digest
    }
    /// Parse one definition on a normalized-definition cache miss.
    pub fn materialize(&self, index: usize) -> Value {
        self.0.tools[index].parse()
    }
    /// Explicitly parse every definition for APIs returning owned raw JSON.
    pub fn materialize_all(&self) -> Vec<Value> {
        self.0.tools.iter().map(|tool| tool.parse()).collect()
    }
    pub fn clear(&mut self) {
        self.0 = Arc::default();
    }
    pub fn push(&mut self, tool: Value) {
        let catalog = Arc::make_mut(&mut self.0);
        let tool = Arc::new(RawTool::new(tool));
        if let Some(name) = &tool.name {
            catalog
                .by_name
                .entry(name.clone())
                .or_insert(catalog.tools.len());
        }
        catalog.tools.push(tool);
    }
    pub fn retain_names(&mut self, mut keep: impl FnMut(Option<&str>) -> bool) {
        let catalog = Arc::make_mut(&mut self.0);
        catalog.tools.retain(|tool| keep(tool.name.as_deref()));
        catalog.index_names();
    }
}
impl From<Vec<Value>> for SerializedTools {
    fn from(values: Vec<Value>) -> Self {
        let mut catalog = SerializedCatalog {
            tools: values
                .into_iter()
                .map(|value| Arc::new(RawTool::new(value)))
                .collect(),
            by_name: HashMap::new(),
        };
        catalog.index_names();
        Self(Arc::new(catalog))
    }
}
impl Serialize for SerializedTools {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len()))?;
        for tool in &self.0.tools {
            seq.serialize_element(&tool.json)?;
        }
        seq.end()
    }
}
impl PartialEq for SerializedTools {
    fn eq(&self, other: &Self) -> bool {
        self.0
            .tools
            .iter()
            .map(|t| t.digest)
            .eq(other.0.tools.iter().map(|t| t.digest))
    }
}
impl PartialEq<Vec<Value>> for SerializedTools {
    fn eq(&self, other: &Vec<Value>) -> bool {
        self.len() == other.len()
            && self
                .0
                .tools
                .iter()
                .zip(other)
                .all(|(tool, value)| tool.parse() == *value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_catalog_shares_bytes_and_preserves_values_on_mutation() {
        let values = vec![
            json!({"name":"read", "inputSchema":{"type":"object", "properties":{"nested":{"type":"object"}}}}),
        ];
        let mut raw = SerializedTools::from(values.clone());
        let prior = raw.clone();
        assert!(Arc::ptr_eq(&raw.0, &prior.0));
        assert_eq!(
            serde_json::to_vec(&raw).unwrap(),
            serde_json::to_vec(&values).unwrap()
        );
        assert_eq!(raw.materialize(0), values[0]);
        raw.push(json!({"name":"next"}));
        assert!(!Arc::ptr_eq(&raw.0, &prior.0));
        assert_eq!(prior.materialize_all(), values);
        assert_eq!(raw.len(), 2);
    }

    #[test]
    fn raw_routing_metadata_preserves_policy_and_app_uri_precedence() {
        let values = vec![
            json!({"name":"delete", "annotations":{"destructiveHint":false}, "destructiveHint":true}),
            json!({"name":"read", "annotations":{"destructiveHint":"invalid"}, "destructiveHint":true}),
            json!({"name":"delete"}),
            json!({"name":"read", "_meta":{"ui":{"resourceUri":7}, "ui/resourceUri":"ui://fallback"}}),
            json!({"name":"read", "_meta":{"ui/resourceUri":"ui://fallback"}}),
        ];
        let raw = SerializedTools::from(values.clone());
        for (i, value) in values.iter().enumerate() {
            assert_eq!(
                raw.policy_metadata(i).destructive,
                crate::router::is_destructive(value)
            );
        }
        assert_eq!(raw.app_uri(3), None);
        assert_eq!(raw.app_uri(4), Some("ui://fallback"));
    }

    #[test]
    fn serialized_metadata_index_tracks_duplicates_and_copy_on_write() {
        let values = vec![
            json!({"name":"read", "inputSchema":{"properties":{"header":{"type":"string","x-mcp-header":"Tenant"}}}}),
            json!({"name":"read", "inputSchema":{"properties":{"header":{"type":"boolean","x-mcp-header":"Other"}}}}),
            json!({"name":"invalid", "inputSchema":{"x-mcp-header":7}}),
        ];
        let mut catalog = SerializedTools::from(values.clone());
        let prior = catalog.clone();
        assert!(catalog.contains_name("read"));
        assert!(!catalog.contains_name("missing"));
        assert_eq!(
            catalog.header_specs("read").unwrap(),
            crate::downstream::header_param_specs(&values[0]).unwrap()
        );
        assert_eq!(
            catalog.header_specs("invalid").unwrap_err(),
            crate::downstream::header_param_specs(&values[2]).unwrap_err()
        );
        assert!(catalog.header_specs("missing").unwrap().is_empty());
        catalog.retain_names(|name| name == Some("invalid"));
        assert!(!catalog.contains_name("read"));
        assert!(prior.contains_name("read"));
        catalog.push(values[1].clone());
        assert_eq!(
            catalog.header_specs("read").unwrap(),
            crate::downstream::header_param_specs(&values[1]).unwrap()
        );
        assert_eq!(
            catalog.materialize_all(),
            vec![values[2].clone(), values[1].clone()]
        );
        assert!(Arc::ptr_eq(&catalog.0.tools[0], &prior.0.tools[2]));
    }

    #[test]
    fn shared_catalog_digests_cover_schema_description_and_order_changes() {
        let values = vec![
            json!({"name":"read", "description":"first", "inputSchema":{"type":"object"}}),
            json!({"name":"other"}),
        ];
        let catalog = SharedTools::from(values.clone());
        let copy = catalog.clone();
        assert!(Arc::ptr_eq(&catalog.0[0], &copy.0[0]));
        assert_eq!(catalog.digests(), values.digests());
        for field in ["description", "inputSchema"] {
            let mut changed = values.clone();
            changed[0][field] = json!("changed");
            assert_ne!(catalog.digests(), changed.digests());
        }
        let mut reordered = values;
        reordered.reverse();
        assert_ne!(catalog.digests(), reordered.digests());
    }
}
