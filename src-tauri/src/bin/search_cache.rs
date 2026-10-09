//! Bounded background indexing with a content-addressed, disposable vector cache.
use super::search_static;
use conduit_lib::tool_definitions::{SharedTools, ToolCatalog};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::sync::{Condvar, Mutex};

#[derive(Debug, Default)]
pub struct Vectors(pub Arc<OnceLock<Vec<Vec<f32>>>>);

struct HashWriter(Sha256);
impl std::io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn key(tool: &Value) -> String {
    let mut hash = HashWriter(Sha256::new());
    hash.0.update(search_static::asset_digest());
    // Include all metadata, including schemas, without allocating a second JSON.
    serde_json::to_writer(&mut hash, tool).expect("JSON tool definition");
    format!("{:x}", hash.0.finalize())
}

fn read(path: &Path, dimensions: usize) -> Option<Vec<f32>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take((dimensions * 4 + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() != dimensions * 4 {
        return None;
    }
    let vector: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    let norm: f32 = vector.iter().map(|v| v * v).sum();
    (vector.iter().all(|v| v.is_finite()) && (norm == 0.0 || (norm - 1.0).abs() < 0.001))
        .then_some(vector)
}

fn write(path: &Path, vector: &[f32]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        for value in vector {
            file.write_all(&value.to_le_bytes())?;
        }
        file.flush()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn encode(documents: &[(String, String)], directory: Option<&Path>) -> (Vec<Vec<f32>>, usize) {
    let model = search_static::model();
    let cache = directory.filter(|dir| std::fs::create_dir_all(dir).is_ok());
    if let Some(directory) = cache {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
        }
        evict(
            directory,
            32 * 1024 * 1024,
            std::time::Duration::from_secs(14 * 86400),
        );
    }
    let mut embedded = 0;
    let vectors = documents
        .iter()
        .map(|(digest, text)| {
            let path = cache.map(|dir| dir.join(digest));
            if let Some(vector) = path
                .as_deref()
                .and_then(|path| read(path, model.dimensions()))
            {
                return vector;
            }
            embedded += 1;
            let vector = model.encode(text);
            if let Some(path) = path {
                let _ = write(&path, &vector);
            }
            vector
        })
        .collect();
    if let Some(directory) = cache {
        evict(
            directory,
            32 * 1024 * 1024,
            std::time::Duration::from_secs(14 * 86400),
        );
    }
    (vectors, embedded)
}

struct Job {
    tools: SharedTools,
    directory: Option<PathBuf>,
    ready: Arc<OnceLock<Vec<Vec<f32>>>>,
    completed: Option<std::sync::mpsc::Sender<()>>,
}
type Queue = Arc<(Mutex<Option<Job>>, Condvar)>;

fn worker() -> Option<Queue> {
    let queue: Queue = Arc::new((Mutex::new(None), Condvar::new()));
    let worker = queue.clone();
    std::thread::Builder::new()
        .name("search-index".into())
        .spawn(move || loop {
            let (lock, wake) = &*worker;
            let mut pending = lock.lock().unwrap_or_else(|e| e.into_inner());
            while pending.is_none() {
                pending = wake.wait(pending).unwrap_or_else(|e| e.into_inner());
            }
            let job = pending.take().unwrap();
            drop(pending);
            // Hashing complete schemas and building model documents belong on this
            // worker. Requests only read the published OnceLock.
            let documents: Vec<_> = job
                .tools
                .iter()
                .map(|tool| (key(tool), search_static::document(tool)))
                .collect();
            let (vectors, _) = encode(&documents, job.directory.as_deref());
            let _ = job.ready.set(vectors);
            if let Some(completed) = job.completed {
                let _ = completed.send(());
            }
        })
        .ok()?;
    Some(queue)
}

fn submit(
    queue: &Queue,
    tools: SharedTools,
    directory: Option<PathBuf>,
    ready: Arc<OnceLock<Vec<Vec<f32>>>>,
    completed: Option<std::sync::mpsc::Sender<()>>,
) {
    let (lock, wake) = &**queue;
    // Only full catalog snapshot construction submits. Scoped requests filter
    // that snapshot and cannot replace its pending job.
    *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(Job {
        tools,
        directory,
        ready,
        completed,
    });
    wake.notify_one();
}

impl Vectors {
    pub fn build(tools: &dyn ToolCatalog) -> Self {
        let result = Self::default();
        if !search_static::enabled() {
            return result;
        }
        #[cfg(test)]
        result
            .0
            .set(tools.iter().map(search_static::tool_vector).collect())
            .unwrap();
        #[cfg(not(test))]
        {
            static WORKER: OnceLock<Option<Queue>> = OnceLock::new();
            if let Some(queue) = WORKER.get_or_init(worker) {
                let directory =
                    conduit_lib::registry::conduit_dir().map(|dir| dir.join("search-vectors"));
                submit(queue, tools.shared(), directory, result.0.clone(), None);
            }
        }
        result
    }
}

// Cache files are disposable. Bound both age and total bytes, independently of
// catalog turnover. Run on the encoder thread, never during a search request.
fn evict(directory: &Path, maximum: u64, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now
            .duration_since(modified)
            .is_ok_and(|elapsed| elapsed > age)
        {
            let _ = std::fs::remove_file(entry.path());
        } else {
            files.push((modified, metadata.len(), entry.path()));
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut size: u64 = files.iter().map(|(_, size, _)| *size).sum();
    for (_, bytes, path) in files {
        if size <= maximum {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            size = size.saturating_sub(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_worker_publishes_and_cache_evicts() {
        let queue = worker().unwrap();
        let root =
            std::env::temp_dir().join(format!("toolport-search-worker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tool = serde_json::json!({"name":"sample__read_record","description":"Read record","inputSchema":{}});
        let ready = Arc::new(OnceLock::new());
        let (send, receive) = std::sync::mpsc::channel();
        submit(
            &queue,
            vec![tool.clone()].into(),
            Some(root.clone()),
            ready.clone(),
            Some(send),
        );
        receive
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        assert_eq!(ready.get().unwrap()[0], search_static::tool_vector(&tool));
        assert!(root.join(key(&tool)).exists());
        evict(&root, 0, std::time::Duration::from_secs(86400));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::write(root.join("old"), b"old").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("old"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        evict(&root, u64::MAX, std::time::Duration::from_secs(86400));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cache_reuses_content_and_recovers_corruption() {
        let root =
            std::env::temp_dir().join(format!("toolport-search-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let first = serde_json::json!({"name":"example__read_record","description":"Read a record","inputSchema":{"properties":{"id":{"type":"string"}}}});
        let document = |tool: &Value| (key(tool), search_static::document(tool));
        let docs = vec![document(&first)];
        let (vectors, count) = encode(&docs, Some(&root));
        assert_eq!(count, 1);
        let (cached, count) = encode(&docs, Some(&root));
        assert_eq!(count, 0);
        assert_eq!(vectors, cached);
        let mut changed = first.clone();
        changed["inputSchema"]["properties"]["id"]["description"] =
            serde_json::json!("Resource identifier");
        assert_ne!(key(&first), key(&changed));
        let (_, count) = encode(&[document(&first), document(&changed)], Some(&root));
        assert_eq!(count, 1);
        std::fs::write(root.join(&docs[0].0), b"broken").unwrap();
        let (recovered, count) = encode(&docs, Some(&root));
        assert_eq!(count, 1);
        assert_eq!(vectors, recovered);
        let (uncached, count) = encode(&docs, None);
        assert_eq!(count, 1);
        assert_eq!(vectors, uncached);
        std::fs::remove_dir_all(root).unwrap();
    }
}
