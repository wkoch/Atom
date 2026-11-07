//! Binário dev-only `atom-conformance` (`conformance.md` §3): roda a suíte
//! de conformidade contra **qualquer** implementação e imprime JSON
//! `[{id, pass, detail?}]`, saindo 0 (todas passam) / 1.
//!
//! ```sh
//! cargo run -p atom-core --example atom-conformance -- ext -- ./minha-ext
//! cargo run -p atom-core --example atom-conformance -- core --socket /tmp/atom.sock
//! ```
// spec: [CF-1]

#[path = "../tests/conformance_common.rs"]
mod common;

use common::{Check, Harness, run_core_suite, run_ext_suite, run_invoke_cases};
use serde_json::Value;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let checks: Vec<Check> = if args.first().is_some_and(|a| a == "ext") {
        let rest: Vec<String> = args
            .iter()
            .skip(1)
            .skip_while(|a| *a == "--")
            .cloned()
            .collect();
        if rest.is_empty() {
            eprintln!("uso: atom-conformance ext -- <comando da extensão>");
            std::process::exit(2);
        }
        let tag = rest.join(" ");
        match Harness::spawn_cmd(&rest).await {
            Ok(mut h) => {
                let mut c = run_ext_suite(&mut h, &tag, None).await;
                c.extend(run_invoke_cases(&mut h, &tag, &[]).await);
                h.shutdown_best_effort().await;
                c
            }
            Err(e) => vec![Check::fail("P-CONN-1", e)],
        }
    } else if args.first().is_some_and(|a| a == "core") {
        let sock = args
            .windows(2)
            .find(|w| w[0] == "--socket")
            .map(|w| PathBuf::from(&w[1]));
        let Some(sock) = sock else {
            eprintln!("uso: atom-conformance core --socket <caminho>");
            std::process::exit(2);
        };
        #[cfg(unix)]
        {
            run_core_suite(&sock, "core").await
        }
        #[cfg(not(unix))]
        {
            let _ = sock;
            vec![Check::fail("P-CONN-2", "modo core exige unix".to_string())]
        }
    } else {
        eprintln!("uso: atom-conformance (ext -- <cmd> | core --socket <path>)");
        std::process::exit(2);
    };
    let arr: Vec<Value> = checks.iter().map(Check::json).collect();
    println!("{}", serde_json::to_string(&arr).unwrap());
    let failed = checks.iter().any(|c| !c.pass);
    std::process::exit(i32::from(failed));
}
