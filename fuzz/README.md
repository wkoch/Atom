# Fuzz (`cargo-fuzz`, nightly só no job de fuzz)

Alvos (conformance.md §5, `[CF-4]`): framing NDJSON, `atom.toml`,
`atom.lock`, `profile.toml`, índice `store/v1`, metadados TUF, extração tar.

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run ndjson -- -max_total_time=1800
```

CI noturno: 30 min por alvo, corpus versionado em `fuzz/corpus/<alvo>/`;
crash = issue bloqueante.
