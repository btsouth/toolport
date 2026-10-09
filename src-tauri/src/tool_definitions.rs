//! Immutable tool definitions shared by router views and gateway catalogs.
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
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
    pub(crate) arguments: Option<Arc<crate::schema_compat::ArgumentMap>>,
    pub digest: [u8; 32],
}
impl ToolDefinition {
    pub(crate) fn with_arguments(
        value: Value,
        arguments: Option<Arc<crate::schema_compat::ArgumentMap>>,
    ) -> Self {
        Self {
            digest: content_digest(&value),
            value,
            arguments,
        }
    }
    pub fn new(value: Value) -> Self {
        Self {
            digest: content_digest(&value),
            value,
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

/// Original downstream catalogs retain canonical bytes, not a second parsed tree.
/// Names and digests let routing and normalization find a definition without parsing.
#[derive(Clone, Debug, Default)]
pub struct DownstreamTools(Arc<Vec<RawTool>>);
#[derive(Debug, Clone)]
struct RawTool {
    name: Option<String>,
    destructive: bool,
    app_uri: Option<String>,
    json: Box<serde_json::value::RawValue>,
    digest: [u8; 32],
}
impl RawTool {
    fn new(value: Value) -> Self {
        Self {
            destructive: crate::router::is_destructive(&value),
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
impl DownstreamTools {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn names(&self) -> Vec<Option<&str>> {
        self.0.iter().map(|tool| tool.name.as_deref()).collect()
    }
    pub fn is_destructive(&self, index: usize) -> bool {
        self.0[index].destructive
    }
    pub fn app_uri(&self, index: usize) -> Option<&str> {
        self.0[index].app_uri.as_deref()
    }
    pub fn digest(&self, index: usize) -> [u8; 32] {
        self.0[index].digest
    }
    pub fn iter(&self) -> impl Iterator<Item = Value> + '_ {
        self.0.iter().map(RawTool::parse)
    }
    pub fn get(&self, index: usize) -> Value {
        self.0[index].parse()
    }
    pub fn named(&self, name: &str) -> Option<Value> {
        self.0
            .iter()
            .find(|t| t.name.as_deref() == Some(name))
            .map(RawTool::parse)
    }
    pub fn to_vec(&self) -> Vec<Value> {
        self.iter().collect()
    }
    pub fn clear(&mut self) {
        self.0 = Arc::default();
    }
    pub fn push(&mut self, tool: Value) {
        Arc::make_mut(&mut self.0).push(RawTool::new(tool));
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&Value) -> bool) {
        Arc::make_mut(&mut self.0).retain(|tool| keep(&tool.parse()));
    }
}
impl From<Vec<Value>> for DownstreamTools {
    fn from(values: Vec<Value>) -> Self {
        Self(Arc::new(values.into_iter().map(RawTool::new).collect()))
    }
}
impl Serialize for DownstreamTools {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len()))?;
        for tool in self.0.iter() {
            seq.serialize_element(&tool.json)?;
        }
        seq.end()
    }
}
impl PartialEq for DownstreamTools {
    fn eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .map(|t| t.digest)
            .eq(other.0.iter().map(|t| t.digest))
    }
}
impl PartialEq<Vec<Value>> for DownstreamTools {
    fn eq(&self, other: &Vec<Value>) -> bool {
        self.iter().eq(other.iter().cloned())
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
        let mut raw = DownstreamTools::from(values.clone());
        let prior = raw.clone();
        assert!(Arc::ptr_eq(&raw.0, &prior.0));
        assert_eq!(
            serde_json::to_vec(&raw).unwrap(),
            serde_json::to_vec(&values).unwrap()
        );
        assert_eq!(raw.named("read"), Some(values[0].clone()));
        raw.push(json!({"name":"next"}));
        assert!(!Arc::ptr_eq(&raw.0, &prior.0));
        assert_eq!(prior.to_vec(), values);
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
        let raw = DownstreamTools::from(values.clone());
        for (i, value) in values.iter().enumerate() {
            assert_eq!(raw.is_destructive(i), crate::router::is_destructive(value));
        }
        assert_eq!(raw.app_uri(3), None);
        assert_eq!(raw.app_uri(4), Some("ui://fallback"));
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
