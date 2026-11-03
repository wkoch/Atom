# Ajustes nos contratos `@0` registrados no dogfood (v0.2)

Contratos continuam `@0` (instáveis, ADR-014). Lista do que a implementação
ensinou — candidata a virar `@1` quando estabilizar.

## Decidido ao implementar (já vale)

1. **`ai.llm@0/complete` com `stream: true` exige resposta imediata `{"stream": id}`.**
   Sem isso o chamador não casa chunks/end (visto na prática: drive que só
   respondia no fim causava deadlock aparente). O `service.invoke` continua
   sendo request com resposta final — o fim do stream (`atom.stream.end`) e a
   resposta final coexistem.
2. **Cancelamento é notificação `atom.stream.cancel {"stream"}` produtor afora.**
   Cada camada (ui → agente → llm) repassa ao seu produtor e aborta o próprio
   stream com erro `-32800`. Ctrl+C no `atom run chat` fecha o TTY → a ui-cli
   cancela upstream antes de sair.
3. **Chunk `data` carrega `delta_text` / `delta_tool_call {id, name, input_delta}`**
   e o resultado final do llm repete os `tool_call` completos (acumulados dos
   deltas). Agentes não precisam reconstruir JSON parcial.
4. **`ai.tools@0/execute` retorna `output: [parts]` + `is_error`**; `shell_exec`
   adiciona `exit_code` (campo extra tolerado — schemas não usam
   `additionalProperties: false`).
5. **`ai.agent@0/run` agrega tools via `atom.registry.list`** (união local, sem
   provider agregador dedicado). `options` ganhou `workdir` (caminhos relativos
   viram absolutos) e `model`/`max_turns` com default no agente (10 turnos).
6. **`ui.prompt@0` sem provider na v0.2**: o agente não pede confirmação; o
   contrato existe para a v0.3 (`guard-shell`). `ask` aceita `schema` opaco.
7. **`sessions.*` do agente é em memória** (morre com o processo). Persistência
   é tema da extensão de memória (futura), não do loop básico.

## Em aberto (para `@1` ou v0.3+)

- Backpressure (`atom.stream.ack` janela 256): aceito e ignorado; core repassa
  imediatamente. Medir antes de implementar pausa real.
- `models()` da Anthropic retorna só o modelo configurado (API sem listagem).
- `usage` padronizado como `{input_tokens, output_tokens}`; cada provider
  mapeia o próprio formato (OpenAI `prompt_tokens/completion_tokens` →
  traduzir no provider).
- Secrets são escopo `(tenant, perfil)` — dois `ATOM_HOME` com mesmo nome de
  perfil compartilham o keyring. Documentar no tutorial de perfis.
- `atom.storage.dir` não implementado na v0.2 (capability `fs.data` é v0.6).
