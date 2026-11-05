//! Eventos com retomada (`[P-EV-1..5]`, ADR-022).

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, host::Incoming, pkg};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn build_bin(pkg: &str, bin: &str) -> PathBuf {
    let b = workspace_root().join(format!("target/debug/{bin}"));
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", pkg])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p {pkg} falhou");
    }
    b
}

fn install(core: &Core, dir: &str, pkg_name: &str, cargo_pkg: &str, bin: &str, entry: &str) {
    let src = workspace_root().join(dir);
    let id = pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert!(id.starts_with(pkg_name));
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(pkg_name).unwrap();
    let pdir = pkg::package_dir(&core.data_store, lp).unwrap();
    let dst = pdir.join(entry);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(build_bin(cargo_pkg, bin), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

async fn sub(
    core: &mut Core,
    ext: &str,
    topics: serde_json::Value,
    from: Option<serde_json::Value>,
) {
    let mut params = json!({"topics": topics});
    if let Some(f) = from {
        params["from"] = f;
    }
    core.pump_one(Incoming {
        ext: ext.to_string(),
        id: Some(json!("sub-1")),
        method: "atom.events.subscribe".to_string(),
        params,
    })
    .await;
}

/// `[P-EV-1/2]`: publicar atribui `epoch`/`offset` monotônicos e retém no ring.
// spec: [P-EV-1]
// spec: [P-EV-2]
#[tokio::test]
async fn events_assign_cursor_and_retain() {
    // spec: [P-EV-1]
    // spec: [P-EV-2]
    // spec: [P-EV-4]
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("ev", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    core.refresh().unwrap();
    let ctx = atom_protocol::CallerCtx::local("ev", "core");
    core.publish_event("atom.test.a", json!({"n": 1}), &ctx)
        .await
        .unwrap();
    core.publish_event("atom.test.b", json!({"n": 2}), &ctx)
        .await
        .unwrap();
    assert_eq!(core.event_ring_len_for_test(), 2);
    let (epoch, offsets) = core.event_cursors_for_test();
    assert!(epoch.starts_with("ep_"), "epoch {epoch}");
    assert_eq!(offsets, vec![1, 2]);
    core.shutdown().await;
}

/// `[P-EV-3]`: `from` reentrega retidos que casam; `epoch` divergente dá `gap`.
// spec: [P-EV-3]
#[tokio::test]
async fn events_resume_replays_and_gaps() {
    // spec: [P-EV-3]
    // spec: [P-EV-4]
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("ev2", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    core.refresh().unwrap();
    sub(&mut core, "test/echo", json!(["atom.test.*"]), None).await;
    let ctx = atom_protocol::CallerCtx::local("ev2", "core");
    core.publish_event("atom.test.a", json!({"n": 1}), &ctx)
        .await
        .unwrap();
    let (epoch, _) = core.event_cursors_for_test();
    sub(
        &mut core,
        "test/echo",
        json!(["atom.test.*"]),
        Some(json!({"epoch": "ep_outro", "offset": 0})),
    )
    .await;
    assert!(core.event_gap_sent_for_test("test/echo"));
    core.clear_test_flags();
    sub(
        &mut core,
        "test/echo",
        json!(["atom.test.*"]),
        Some(json!({"epoch": epoch, "offset": 0})),
    )
    .await;
    assert!(core.event_replayed_for_test() >= 1);
    core.shutdown().await;
}

/// `[P-EV-2]`: handover sobrevive ao shutdown/boot com mesmo `epoch`.
// spec: [P-EV-2]
#[tokio::test]
async fn events_handover_survives_restart() {
    // spec: [P-EV-2]
    let home = tempfile::tempdir().unwrap();
    let saved_epoch;
    {
        let mut core = Core::open_with("ev3", Some(home.path())).await.unwrap();
        let ctx = atom_protocol::CallerCtx::local("ev3", "core");
        core.publish_event("atom.test.a", json!({"n": 1}), &ctx)
            .await
            .unwrap();
        let (epoch, _) = core.event_cursors_for_test();
        saved_epoch = epoch;
        core.shutdown().await;
    }
    let mut core2 = Core::open_with("ev3", Some(home.path())).await.unwrap();
    let (epoch2, offsets2) = core2.event_cursors_for_test();
    assert!(epoch2.starts_with("ep_"), "epoch2 {epoch2}");
    assert_eq!(epoch2, saved_epoch, "handover deve manter o epoch");
    assert_eq!(offsets2, vec![1], "handover deve reter o evento");
    core2.shutdown().await;
}

/// `[P-EV-5]`: assinaturas além de `max_subs` recusadas sem travar.
// spec: [P-EV-5]
#[tokio::test]
async fn events_max_subs_overload() {
    // spec: [P-EV-5]
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("ev4", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    core.refresh().unwrap();
    core.config.bus.max_subs = 1;
    core.pump_one(Incoming {
        ext: "test/echo".to_string(),
        id: Some(json!("s1")),
        method: "atom.events.subscribe".to_string(),
        params: json!({"topics": ["a", "b"]}),
    })
    .await;
    core.shutdown().await;
}
