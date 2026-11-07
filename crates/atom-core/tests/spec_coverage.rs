//! Cobertura dos mecanismos de verificação (`conformance.md` §1/§5):
//! o script `scripts/spec-ids` roda e fica verde; o workflow de mutação
//! existe e o escopo crítico está documentado.
// spec: [CF-2]
// spec: [CF-5]

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

// spec: [CF-2]
#[test]
fn spec_ids_script_is_green() {
    let out = std::process::Command::new("sh")
        .arg(
            workspace_root()
                .join("scripts/spec-ids")
                .to_string_lossy()
                .to_string(),
        )
        .output()
        .expect("scripts/spec-ids executável");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "spec-ids falhou:\n{stdout}");
    assert!(
        stdout.contains("OK: spec-ids verde"),
        "sem selo verde:\n{stdout}"
    );
}

// spec: [CF-5]
#[test]
fn mutants_workflow_covers_critical_scope() {
    let yml = std::fs::read_to_string(workspace_root().join(".github/workflows/mutants.yml"))
        .expect("mutants.yml existe");
    for scope in ["bus.rs", "store", "tuf", "ctx"] {
        assert!(
            yml.contains(scope),
            "escopo crítico ausente no mutants.yml: {scope}"
        );
    }
}
