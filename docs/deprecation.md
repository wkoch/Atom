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
- Contratos de IA (`ai.*@0`, `ui.*@0`) **não** congelam: evoluem `@0` → `@1`
  sem release do core (ADR-014). Quebras são anunciadas em
  `docs/contract-adjustments-*.md`.
