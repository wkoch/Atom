#![forbid(unsafe_code)]
//! Esqueleto de fuzz (`conformance.md §5`): alvos existem, corpus versionado.
// spec: [CF-4]

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

// spec: [CF-4]
#[test]
fn fuzz_skeleton_targets_and_corpus() {
    // Seis alvos exigidos pela spec: NDJSON, atom.toml, atom.lock,
    // profile.toml, índice store/v1, extração tar.
    for target in [
        "ndjson",
        "manifest",
        "lockfile",
        "profile",
        "store_index",
        "tar_extract",
    ] {
        let rs = workspace_root()
            .join("fuzz")
            .join("fuzz_targets")
            .join(format!("{target}.rs"));
        assert!(rs.exists(), "alvo de fuzz ausente: {}", rs.display());
        let corpus = workspace_root().join("fuzz").join("corpus").join(target);
        assert!(
            corpus.is_dir(),
            "corpus versionado ausente: {}",
            corpus.display()
        );
    }
    assert!(workspace_root().join("fuzz/README.md").exists());
    assert!(workspace_root().join("fuzz/Cargo.toml").exists());
}
