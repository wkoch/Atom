# Dogfood v0.2 — chat real com o Atom

Pré-requisito: core v0.1 + extensões desta versão instaladas com `--dev`.

## 1. Instalar tudo

```sh
export ATOM_HOME=~/.atom-dogfood   # ou use o default
atom ext install --dev extensions/contracts/ai-llm
atom ext install --dev extensions/contracts/ai-tools
atom ext install --dev extensions/contracts/ai-agent
atom ext install --dev extensions/contracts/ui-prompt
atom ext install --dev extensions/llm-openai-compat   # ou llm-anthropic / Ollama local
atom ext install --dev extensions/tools-fs
atom ext install --dev extensions/tools-shell
atom ext install --dev extensions/agent-loop-basic
atom ext install --dev extensions/ui-cli
```

## 2. Chave de API e binding

```sh
atom secret set openai_api_key < ~/.openai-key
atom config set atom/llm-openai-compat model gpt-4o-mini
atom bind ai.llm@0 atom/llm-openai-compat
atom bind ai.agent@0 atom/agent-loop-basic
```

Sem chave (Ollama/LM Studio local):

```sh
atom config set atom/llm-openai-compat base_url http://localhost:11434/v1
atom config set atom/llm-openai-compat model qwen2.5-coder
atom bind ai.llm@0 atom/llm-openai-compat
```

Trocar de provider é só o `bind` (aceite da v0.2).

## 3. Chat

```sh
atom run chat
```

Comandos no chat: qualquer texto é tarefa; `exit` sai. O agente usa
`fs_read/fs_write/fs_list/shell_exec` e mostra o que fez. Ctrl+C cancela o
stream atual fim a fim (o llm aborta a requisição HTTP).

Debug sem UI: `atom call --stream ai.agent@0 run '{"session":"t","input":"..."}'`.

## Limites conhecidos da v0.2

- `atom run` é sempre em processo (sem daemon); `atom serve` não faz relay.
- Backpressure de streams não aplicada; janela de 256 acks documentada, não fiscalizada.
- Secrets caem para arquivo 0600 se o keyring falhar; nomes listáveis, valores nunca.
- `atom logs` ainda é stub (ver stderr do `serve`).
- Sem sandbox de paths nas tools (capabilities entram na v0.6).
