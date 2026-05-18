use crate::{settings, types::LogEntry};
use chrono::Utc;
use parking_lot::Mutex;
use std::{collections::VecDeque, fs::OpenOptions, io::Write, path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct AuditLogger {
    inner: Arc<Inner>,
}

struct Inner {
    recent: Mutex<VecDeque<LogEntry>>,
    path: PathBuf,
    max_bytes: u64,
}

impl AuditLogger {
    pub fn new(max_bytes: u64) -> Self {
        let dir = settings::config_dir().join("logs");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            inner: Arc::new(Inner {
                recent: Mutex::new(VecDeque::with_capacity(300)),
                path: dir.join("audit.jsonl"),
                max_bytes,
            }),
        }
    }

    pub fn log(&self, level: impl Into<String>, message: impl Into<String>) {
        let entry = LogEntry {
            ts: Utc::now().to_rfc3339(),
            level: level.into(),
            message: message.into(),
        };

        {
            let mut recent = self.inner.recent.lock();
            recent.push_front(entry.clone());
            while recent.len() > 250 {
                recent.pop_back();
            }
        }

        self.rotate_if_needed();
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.path)
        {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    pub fn recent(&self) -> Vec<LogEntry> {
        self.inner.recent.lock().iter().cloned().collect()
    }

    fn rotate_if_needed(&self) {
        let Ok(meta) = std::fs::metadata(&self.inner.path) else {
            return;
        };
        if meta.len() < self.inner.max_bytes {
            return;
        }
        let rotated = self.inner.path.with_extension("jsonl.1");
        let _ = std::fs::rename(&self.inner.path, rotated);
    }
}
