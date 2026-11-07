# Política de depreciação (v1.0)

## Congelado

- Protocolo `atom/1` (envelope, handshake, `service.invoke`, streams,
  interceptors, eventos, kind, `atom.*` — `docs/specs/protocol.md`).
- Formatos: manifesto (`manifest = 1`), lockfile (`lock = 1`),
  `profile.toml` (`profile = 1`), loja `store/v1`.
- Leitores rejeitam versão de formato maior que a suportada
  (`manifest`, `lockfile`, `profile`); extensões com
  `protocol != "atom/1"` são recusadas (`-32001`).

## Regras de evolução

- `atom/1` só recebe adições compatíveis (métodos e campos opcionais novos;
  desconhecidos devem ser ignorados pelos dois lados).
- Mudança incompatível = `atom/2`; o core suporta `atom/1` e `atom/2`
  simultaneamente por **pelo menos um major após `atom/2`**.
- Mesma regra para `manifest`/`lock`/`profile`/`store/v1` (bump de versão
  + suporte dual pelo mesmo período).
- v1.1: adições a `atom/1` são opt-in por presença de campo (`deadline`, `flow`, `from`, `if_version`); sem eles o comportamento é o da v1.0. Exceção registrada (ADR-020): a janela de 256 chunks por ack da v1.0, nunca implementada, vira opt-in. Exceção de segurança (ADR-019): peer descendente do daemon deixa de ser cliente confiável (`-32005`).
- `store/v1` → `store/v2` (ADR-024): cliente suporta ambos por pelo menos um major após o lançamento de `store/v2`; sources v1 exibem aviso durante todo o período.
- Contratos de IA (`ai.*@0`, `ui.*@0`) **não** congelam: evoluem `@0` → `@1`
  sem release do core (ADR-014). Quebras são anunciadas em
  `docs/contract-adjustments-*.md`.
