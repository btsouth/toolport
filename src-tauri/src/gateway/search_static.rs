//! Bundled, quantized Model2Vec inference. No endpoint, GPU or runtime transformer.
use serde_json::Value;
use std::borrow::Cow;
use std::sync::OnceLock;
use tokenizers::Tokenizer;

const WEIGHTS: &[u8] = include_bytes!("../../assets/search/model-q8.bin");
const TOKENIZER: &[u8] = include_bytes!("../../assets/search/tokenizer.json");

pub struct Model {
    tokenizer: Tokenizer,
    weights: Cow<'static, [u8]>,
    rows: usize,
    dimensions: usize,
    bits: usize,
}

impl Model {
    fn load(weights: Cow<'static, [u8]>, tokenizer: &[u8]) -> Result<Self, String> {
        if weights.len() < 20 || &weights[..8] != b"TPSEMQ01" {
            return Err("invalid static search model header".into());
        }
        let field =
            |offset| u32::from_le_bytes(weights[offset..offset + 4].try_into().unwrap()) as usize;
        let (rows, dimensions, bits) = (field(8), field(12), field(16));
        if rows == 0 || dimensions == 0 || dimensions > 1024 || ![4, 8].contains(&bits) {
            return Err("invalid static search model shape".into());
        }
        let size = rows
            .checked_mul(dimensions)
            .and_then(|n| n.checked_mul(bits))
            .and_then(|n| n.checked_div(8))
            .and_then(|n| n.checked_add(rows * 4 + 20));
        if !size.is_some_and(|size| {
            weights.len() >= size
                && (weights.len() == size
                    || weights[size..] == *include_bytes!("../../assets/search/LICENSE"))
        }) || (bits == 4 && dimensions % 2 != 0)
        {
            return Err("invalid static search model length".into());
        }
        for bytes in weights[20..20 + rows * 4].chunks_exact(4) {
            let scale = f32::from_le_bytes(bytes.try_into().unwrap());
            if !scale.is_finite() || scale <= 0.0 {
                return Err("invalid static search model scale".into());
            }
        }
        let tokenizer = Tokenizer::from_bytes(tokenizer).map_err(|e| e.to_string())?;
        if tokenizer.get_vocab_size(false) != rows {
            return Err("static search vocabulary mismatch".into());
        }
        Ok(Self {
            tokenizer,
            weights,
            rows,
            dimensions,
            bits,
        })
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn encode(&self, text: &str) -> Vec<f32> {
        let mut out = vec![0.0; self.dimensions];
        let bounded: String = text.chars().take(2048).collect();
        let Ok(encoded) = self.tokenizer.encode(bounded, false) else {
            return out;
        };
        for id in encoded.get_ids().iter().take(256).map(|id| *id as usize) {
            if id >= self.rows {
                continue;
            }
            let scale =
                f32::from_le_bytes(self.weights[20 + id * 4..24 + id * 4].try_into().unwrap());
            let width = self.dimensions * self.bits / 8;
            let start = 20 + self.rows * 4 + id * width;
            let row = &self.weights[start..start + width];
            if self.bits == 8 {
                for (value, byte) in out.iter_mut().zip(row) {
                    *value += *byte as i8 as f32 * scale;
                }
            } else {
                for (values, byte) in out.chunks_exact_mut(2).zip(row) {
                    values[0] += ((*byte & 15) as f32 - 8.0) * scale;
                    values[1] += ((*byte >> 4) as f32 - 8.0) * scale;
                }
            }
        }
        let norm = out.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut out {
                *v /= norm;
            }
        }
        out
    }
}

pub fn asset_digest() -> &'static [u8] {
    use sha2::{Digest, Sha256};
    static DIGEST: OnceLock<Vec<u8>> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut hash = Sha256::new();
        hash.update(b"search-document-v1");
        hash.update(WEIGHTS);
        hash.update(TOKENIZER);
        hash.finalize().to_vec()
    })
}

pub fn model() -> &'static Model {
    static MODEL: OnceLock<Model> = OnceLock::new();
    MODEL.get_or_init(|| {
        #[cfg(test)]
        if let Some(path) = std::env::var_os("SEARCH_STATIC_MODEL_DIR") {
            let path = std::path::PathBuf::from(path);
            return Model::load(
                Cow::Owned(std::fs::read(path.join("model.bin")).unwrap()),
                &std::fs::read(path.join("tokenizer.json")).unwrap(),
            )
            .expect("benchmark static model");
        }
        Model::load(Cow::Borrowed(WEIGHTS), TOKENIZER)
            .expect("validated bundled static search model")
    })
}

/// Preserve ordinary words rather than lexical stems. Bound verbose API documents
/// to the first sentence so parameter prose cannot drown out the operation.
pub fn document(tool: &Value) -> String {
    let name = tool["name"].as_str().unwrap_or("");
    let mut readable = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if previous_lower && c.is_uppercase() {
            readable.push(' ');
        }
        readable.push(if c == '_' || c == '-' { ' ' } else { c });
        previous_lower = c.is_lowercase();
    }
    let description = tool["description"].as_str().unwrap_or("");
    let end = description
        .char_indices()
        .find_map(|(i, c)| {
            (['.', '!', '?'].contains(&c)
                && description[i + c.len_utf8()..].starts_with(char::is_whitespace))
            .then_some(i)
        })
        .unwrap_or(description.len());
    let description: String = description[..end].chars().take(512).collect();
    format!("{readable} . {description}")
}

// Explicit benchmark vectors are generated by the standalone in-process Rust
// transformer harness. This loader is absent from production binaries.
#[cfg(test)]
fn benchmark_vectors() -> Option<&'static Value> {
    static VECTORS: OnceLock<Option<Value>> = OnceLock::new();
    VECTORS
        .get_or_init(|| {
            std::env::var_os("SEARCH_SENTENCE_VECTORS").map(|path| {
                serde_json::from_slice(&std::fs::read(path).expect("sentence benchmark vectors"))
                    .unwrap()
            })
        })
        .as_ref()
}

#[cfg(test)]
pub fn tool_vector(tool: &Value) -> Vec<f32> {
    #[cfg(test)]
    if let Some(vectors) = benchmark_vectors() {
        return serde_json::from_value(
            vectors["documents"][tool["name"].as_str().unwrap()].clone(),
        )
        .expect("benchmark tool vector");
    }
    model().encode(&document(tool))
}

pub fn query_vector(query: &str) -> Vec<f32> {
    #[cfg(test)]
    if let Some(vectors) = benchmark_vectors() {
        return serde_json::from_value(vectors["queries_vectors"][query].clone())
            .expect("benchmark query vector");
    }
    model().encode(query)
}

pub fn cosine(left: &[f32], right: &[f32]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_search_vectors_are_normalized_and_deterministic() {
        let m = model();
        let v = m.encode("record working hours for a ticket");
        assert_eq!(v, m.encode("record working hours for a ticket"));
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-5);
        assert_eq!(cosine(&m.encode(""), &v), 0.0);
        let time = m.encode("add an issue worklog");
        assert!(cosine(&v, &time) > cosine(&v, &m.encode("purchase a website domain")));
    }
    #[test]
    fn static_search_loader_rejects_invalid_assets() {
        assert!(Model::load(Cow::Borrowed(b"bad"), TOKENIZER).is_err());
        let mut broken = WEIGHTS.to_vec();
        broken[20..24].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(Model::load(Cow::Owned(broken), TOKENIZER).is_err());
        assert_eq!(
            document(
                &serde_json::json!({"name":"agenda__createEvent","description":"Create an event. Extra fields."})
            ),
            "agenda  create Event . Create an event"
        );
    }
}

// Benchmark controls never enter the shipped gateway.
pub fn enabled() -> bool {
    #[cfg(test)]
    if std::env::var_os("SEARCH_LEXICAL_ONLY").is_some() {
        return false;
    }
    true
}
