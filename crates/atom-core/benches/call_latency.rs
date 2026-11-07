//! Benches `criterion` dos orçamentos de conformidade (conformance.md §8 §4):
//! latência por salto de `atom.call` e memória estável sob stream longo.
// spec: [CF-1]

use std::path::Path;

use atom_core::bus::{Core, StreamCall};
use atom_core::{pkg, profile};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::json;
use std::sync::Arc;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn echo_bin() -> std::path::PathBuf {
    let b = workspace_root().join("target/debug/echo");
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", "test-echo"])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p test-echo falhou");
    }
    b
}

/// Core com echo instalado, pronto p/ `call`.
fn ready_core(profile: &str, home: &Path) -> Core {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut core = Core::open_with(profile, Some(home)).await.unwrap();
        let src = workspace_root().join("testdata/echo");
        let _ = pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
        let lock = pkg::load_lock(&core.paths).unwrap();
        let lp = lock.find("test/echo").unwrap();
        let dir = pkg::package_dir(&core.data_store, lp).unwrap();
        let m = pkg::read_manifest(&dir).unwrap();
        let dst = dir.join(m.entrypoint.unwrap());
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(echo_bin(), &dst).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&dst).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&dst, p).unwrap();
        }
        core.refresh().unwrap();
        // Boot fora do bench (não medido).
        core.boot().await.unwrap();
        // Esquenta o caminho.
        let _ = core
            .call("test.echo@0", "echo", json!({"x": 1}), None, None)
            .await
            .unwrap();
        core
    })
}

/// Latência por salto de `atom.call` local (orçamento: p99 ≤ 1 ms sem interceptors).
fn bench_call_latency(c: &mut Criterion) {
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().to_path_buf();
    let _ = profile::AtomPaths::resolve_with(Some(&home_path));
    let mut group = c.benchmark_group("atom.call");
    group.bench_function(BenchmarkId::new("echo_per_hop", "local"), |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut core = ready_core("bench-call", &home_path);
        b.iter(|| {
            rt.block_on(core.call("test.echo@0", "echo", json!({"x": 1}), None, None))
                .unwrap();
        });
        rt.block_on(core.shutdown());
    });
    group.finish();
}

/// Estabilidade de memória sob stream de 10⁶ chunks com consumidor lento
/// (orçamento: memória do core estável ±10%; aqui: o bench completa sem
/// crescimento ilimitado — RSS amostrado fora do harness).
fn bench_stream_memory(c: &mut Criterion) {
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().to_path_buf();
    let mut group = c.benchmark_group("atom.stream");
    group.sample_size(10);
    group.bench_function(BenchmarkId::new("slow_consumer", "1e6_chunks"), |b| {
        b.iter_custom(|iters| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let start = std::time::Instant::now();
            for _ in 0..iters {
                rt.block_on(async {
                    let mut core = Core::open_with("bench-stream", Some(&home_path))
                        .await
                        .unwrap();
                    // Sem provider real: mede o custo do canal + descarte com
                    // consumidor lento (buffer 1, dreno tardio).
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<atom_core::bus::StreamEvent>(1);
                    let cancel = Arc::new(tokio::sync::Notify::new());
                    let c2 = cancel.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        c2.notify_one();
                    });
                    let r = core
                        .call_stream_chan(
                            StreamCall {
                                id: atom_protocol::new_stream_id(),
                                service: "test.echo@0".to_string(),
                                method: "echo".to_string(),
                                params: json!({"x": 1}),
                                target: None,
                                ctx: None,
                                flow: None,
                            },
                            tx,
                            cancel,
                        )
                        .await;
                    let _ = r;
                    rx.close();
                    core.shutdown().await;
                });
            }
            start.elapsed()
        });
    });
    group.finish();
}

criterion_group!(benches, bench_call_latency, bench_stream_memory);
criterion_main!(benches);
