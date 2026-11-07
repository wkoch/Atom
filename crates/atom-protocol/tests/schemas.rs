#![forbid(unsafe_code)]
//! Round-trip dos exemplos de `docs/specs/schemas/atom-1/` ([CF-3]).
//!
//! Para cada schema com `params`/`result`/`examples`: cada `example.params`
//! desserializa no tipo serde correspondente e reserializa válido pelo schema
//! (`jsonschema`, já dependência do workspace). O mesmo vale para `result`
//! quando presente (desserializa em `serde_json::Value` e valida no subschema
//! `result` quando ele é um schema — alguns schemas documentam `result` só em
//! texto).
// spec: [CF-3]

use std::path::PathBuf;

use serde::de::DeserializeOwned;

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("specs")
        .join("schemas")
        .join("atom-1")
}

/// Valida `instance` contra `schema` (raiz ou subcaminho).
fn assert_valid(schema: &serde_json::Value, instance: &serde_json::Value, what: &str) {
    if let Err(e) = jsonschema::validate(schema, instance) {
        panic!("{what} inválido pelo schema: {e}\ninstância: {instance}");
    }
}

/// Roda os exemplos de um arquivo de schema contra um tipo serde e valida
/// `params` + `result` no schema.
fn check_file<T>(file: &str)
where
    T: DeserializeOwned + serde::Serialize,
{
    let text = std::fs::read_to_string(schemas_dir().join(file)).unwrap_or_else(|e| {
        panic!("lendo schema {file}: {e}");
    });
    let schema: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse de {file}: {e}"));
    let examples = schema
        .get("examples")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !examples.is_empty(),
        "schema {file} sem examples (exige [CF-3])"
    );
    for (i, ex) in examples.iter().enumerate() {
        let what = format!("{file} exemplo {i}");
        let params = ex.get("params").unwrap_or(&serde_json::Value::Null);
        // Desserializa no tipo serde e reserializa.
        let typed: T = serde_json::from_value(params.clone())
            .unwrap_or_else(|e| panic!("{what}: params não desserializa: {e}\n{params}"));
        let back = serde_json::to_value(&typed).expect("{what}: reserialização");
        // Params reserializados validam no schema raiz (que descreve params).
        assert_valid(&schema, &back, &format!("{what} params"));
        // Result valida no subschema `result` quando ele é um schema.
        if let (Some(result), Some(sub)) = (ex.get("result"), schema.get("result"))
            && sub.is_object()
            && sub.get("type").is_some()
        {
            let r: serde_json::Value = serde_json::from_value(result.clone())
                .unwrap_or_else(|e| panic!("{what}: result inválido: {e}"));
            assert_valid(sub, &r, &format!("{what} result"));
        }
    }
}

// spec: [CF-3]
#[test]
fn schemas_round_trip_cf3() {
    check_file::<atom_protocol::AtomCallParams>("atom.call.json");
    check_file::<atom_protocol::ProceedParams>("atom.proceed.json");
    check_file::<atom_protocol::ServiceInvokeParams>("service.invoke.json");
    check_file::<atom_protocol::InterceptorInvokeParams>("interceptor.invoke.json");
    check_file::<atom_protocol::EventDeliver>("event.deliver.json");
    check_file::<atom_protocol::StreamChunk>("atom.stream.chunk.json");
    check_file::<atom_protocol::StreamEnd>("atom.stream.end.json");
    check_file::<atom_protocol::StreamAck>("atom.stream.ack.json");
    check_file::<atom_protocol::StreamCancel>("atom.stream.cancel.json");
    check_file::<atom_protocol::StreamCredit>("atom.stream.credit.json");
    check_file::<atom_protocol::EventsSubscribeParams>("atom.events.subscribe.json");
    check_file::<atom_protocol::CancelRequestParams>("cancel-request.json");
    check_file::<atom_protocol::CallerCtx>("ctx.json");

    // Respostas de interceptor.invoke: formas do fio do schema.
    let raw = std::fs::read_to_string(schemas_dir().join("interceptor.invoke.json"))
        .expect("lendo interceptor.invoke.json");
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("parse");
    let result_schema = schema.get("result").cloned().expect("result subschema");
    for (i, ex) in schema["examples"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let what = format!("interceptor.invoke.json exemplo {i} result");
        let result = ex.get("result").expect("exemplo sem result");
        let typed: atom_protocol::InterceptorResult = serde_json::from_value(result.clone())
            .unwrap_or_else(|e| panic!("{what}: não desserializa: {e}\n{result}"));
        let back = serde_json::to_value(&typed).expect("{what}: reserialização");
        assert_valid(&result_schema, &back, &what);
    }

    // Erros: códigos da tabela §11.
    let raw =
        std::fs::read_to_string(schemas_dir().join("errors.json")).expect("lendo errors.json");
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("parse");
    for (i, ex) in schema["examples"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let what = format!("errors.json exemplo {i}");
        let params = ex.get("params").expect("exemplo sem params");
        let typed: atom_protocol::RpcError = serde_json::from_value(params.clone())
            .unwrap_or_else(|e| panic!("{what}: não desserializa: {e}\n{params}"));
        let back = serde_json::to_value(&typed).expect("{what}: reserialização");
        assert_valid(&schema, &back, &what);
    }
}
