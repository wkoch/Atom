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

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(t: &tempfile::TempDir) -> crate::profile::ProfilePaths {
        crate::profile::ProfilePaths {
            name: "dev".to_string(),
            dir: t.path().join("cfg"),
            profile_toml: t.path().join("cfg/profile.toml"),
            lock: t.path().join("cfg/atom.lock"),
            lock_prev: t.path().join("cfg/prev"),
            grants: t.path().join("cfg/grants.toml"),
            data_dir: t.path().join("data"),
            kv_dir: t.path().join("data/kv"),
            socket: t.path().join("atom-dev.sock"),
            pid_file: t.path().join("atom-dev.pid"),
            serve_token: t.path().join("data/serve.token"),
        }
    }

    #[test]
    fn disabled_is_noop() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&paths_in(&t), false);
        assert!(!j.enabled());
        j.call("t", "c", "s", "m", true, 0);
        j.event("a.b", "x");
        j.grant("p", "w");
        assert!(!t.path().join("data/journal/bus.jsonl").exists());
    }

    #[test]
    fn enabled_appends_jsonl() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(&paths_in(&t), true);
        assert!(j.enabled());
        j.call("t1", "cli", "test.echo@0", "echo", true, 0);
        j.event("atom.x", "core");
        j.grant("a/b", "service:s");
        let raw = std::fs::read_to_string(t.path().join("data/journal/bus.jsonl")).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 3);
        let kinds: Vec<String> = lines
            .iter()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"].to_string())
            .collect();
        assert_eq!(kinds, vec!["\"call\"", "\"event\"", "\"grant\""]);
    }
}
