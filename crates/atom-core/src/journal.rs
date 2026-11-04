//! Journal opcional do bus (v0.6): append-only JSONL em `<data>/journal/`.
//! Best-effort: erro de escrita nunca falha a chamada. Rotação simples:
//! ao abrir, se `bus.jsonl` passa de 10 MiB, vira `bus.prev.jsonl` (guarda 1).

#![forbid(unsafe_code)]

use std::path::PathBuf;

const MAX_BYTES: u64 = 10 * 1024 * 1024;

fn stamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Journal do bus. `None` interno = desabilitado (custo zero).
pub struct Journal {
    file: Option<PathBuf>,
}

impl Journal {
    pub fn open(paths: &crate::profile::ProfilePaths, enabled: bool) -> Self {
        if !enabled {
            return Self { file: None };
        }
        let dir = paths.data_dir.join("journal");
        if std::fs::create_dir_all(&dir).is_err() {
            return Self { file: None };
        }
        let cur = dir.join("bus.jsonl");
        if std::fs::metadata(&cur).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            let _ = std::fs::rename(&cur, dir.join("bus.prev.jsonl"));
        }
        Self { file: Some(cur) }
    }

    pub fn enabled(&self) -> bool {
        self.file.is_some()
    }

    fn append(&self, v: &serde_json::Value) {
        if let Some(path) = &self.file {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "{}", serde_json::to_string(v).unwrap_or_default());
            }
        }
    }

    pub fn call(
        &self,
        trace: &str,
        caller: &str,
        service: &str,
        method: &str,
        ok: bool,
        code: i32,
    ) {
        if self.file.is_none() {
            return;
        }
        self.append(&serde_json::json!({
            "ts": stamp_secs(), "kind": "call",
            "trace": trace, "caller": caller,
            "service": service, "method": method,
            "ok": ok, "code": code,
        }));
    }

    pub fn event(&self, topic: &str, from: &str) {
        if self.file.is_none() {
            return;
        }
        self.append(&serde_json::json!({
            "ts": stamp_secs(), "kind": "event",
            "topic": topic, "from": from,
        }));
    }

    pub fn grant(&self, package: &str, what: &str) {
        if self.file.is_none() {
            return;
        }
        self.append(&serde_json::json!({
            "ts": stamp_secs(), "kind": "grant",
            "package": package, "what": what,
        }));
    }
}
