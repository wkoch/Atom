//! Soak de backpressure (`conformance.md` §8, `[P-LIM-2]`):
//! 10⁶ chunks de um produtor real através de canal de 1 slot com consumidor
//! lento — memória do core estável e nenhuma perda (`seq` contíguo).
//!
//! Aceite v1.1: "memória estável e sem perda" com 10⁶ chunks e consumidor lento.
// spec: [P-LIM-2]
// spec: [P-STREAM-4]
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use atom_core::bus::{Core, StreamCall};

const CHUNKS: u64 = 1_000_000;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn flood_bin() -> PathBuf {
    let b = {
        let p = workspace_root().join("target/debug/flood");
        if cfg!(windows) {
            p.with_extension("exe")
        } else {
            p
        }
    };
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", "test-flood"])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p test-flood falhou");
    }
    b
}

fn install(core: &Core) {
    let src = workspace_root().join("testdata/flood");
    let id = atom_core::pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert!(id.starts_with("test/flood"));
    let lock = atom_core::pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find("test/flood").unwrap();
    let dir = atom_core::pkg::package_dir(&core.data_store, lp).unwrap();
    let dst = dir.join("bin/flood");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(flood_bin(), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

/// RSS em bytes (Linux, `/proc/self/statm`; `None` nas demais plataformas).
fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let t = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident: u64 = t.split_whitespace().nth(1)?.parse().ok()?;
        Some(resident * 4096)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// `[P-LIM-2]`: 10⁶ chunks, canal de 1 slot, consumidor lento —
/// `seq` 0..N contíguo (sem perda) e RSS estável.
// spec: [P-LIM-2]
// spec: [P-STREAM-4]
#[tokio::test]
async fn stream_soak_million_chunks_slow_consumer() {
    // spec: [P-LIM-2]
    // spec: [P-STREAM-4]
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    {
        let a = atom_core::profile::AtomPaths::resolve_with(Some(home));
        a.ensure().unwrap();
        let p = atom_core::profile::ProfilePaths::new(&a, "soak").unwrap();
        let mut cfg = atom_core::profile::load_or_create(&p).unwrap();
        cfg.bindings
            .insert("test.flood@0".to_string(), "test/flood".to_string());
        atom_core::profile::save(&p, &cfg).unwrap();
    }
    let mut core = Core::open_with("soak", Some(home)).await.unwrap();
    install(&core);
    core.refresh().unwrap();

    // Canal de 1 slot: o `deliver` bloqueia até o consumidor drenar
    // (backpressure fim a fim); consumidor lento de propósito.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<atom_core::bus::StreamEvent>(1);
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let drain = tokio::spawn(async move {
        let mut count: u64 = 0;
        let mut saw_end = false;
        while let Some(ev) = rx.recv().await {
            if ev.method == "atom.stream.chunk" {
                let seq = ev.params.get("seq").and_then(|s| s.as_u64()).unwrap();
                assert_eq!(seq, count, "seq fora de ordem (perda ou duplicata)");
                count += 1;
                // Consumidor lento: cede a cada chunk (o canal de 1 slot
                // força o produtor ao ritmo do dreno).
                tokio::task::yield_now().await;
            } else {
                assert_eq!(ev.method, "atom.stream.end");
                saw_end = true;
            }
        }
        (count, saw_end)
    });

    let before = rss_bytes();
    let r = core
        .call_stream_chan(
            StreamCall {
                id: atom_protocol::new_stream_id(),
                service: "test.flood@0".to_string(),
                method: "run".to_string(),
                params: serde_json::json!({"chunks": CHUNKS}),
                target: None,
                ctx: None,
                flow: None,
            },
            tx,
            cancel,
        )
        .await
        .expect("soak devia completar");
    let after = rss_bytes();
    assert_eq!(r["chunks"], serde_json::json!(CHUNKS));
    let (count, saw_end) = drain.await.unwrap();
    assert!(saw_end, "sem atom.stream.end");
    assert_eq!(count, CHUNKS, "perda: {count} de {CHUNKS}");
    // Memória estável: 10⁶ chunks pelo canal de 1 slot não acumulam.
    // Teto generoso (64 MiB) — o esperado é ~0; só Linux mede.
    if let (Some(b), Some(a)) = (before, after) {
        let growth = a.saturating_sub(b);
        assert!(
            growth < 64 * 1024 * 1024,
            "RSS cresceu {growth} bytes em 10⁶ chunks ([P-LIM-2])"
        );
    }
    core.shutdown().await;
}
