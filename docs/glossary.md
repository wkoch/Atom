# Glossário

- **Core / kernel**: o binário `atom`. Sem conceitos de IA.
- **Pacote**: unidade instalável com `atom.toml`. Pode ser executável ou declarativo.
- **Extensão**: pacote executável em execução (processo) conectado ao bus.
- **kind**: categoria livre do pacote (`skill`, `mcp`, `contract`, `runtime`, `distribution`, `ui`, ...). O core não interpreta.
- **Kind handler**: extensão que carrega pacotes de um `kind` (`handles`).
- **Contrato**: interface (métodos + eventos em JSON Schema) identificada por `nome@major`.
- **Provider**: extensão que implementa um contrato (`provides`).
- **Binding**: escolha, no perfil, de qual provider atende um contrato.
- **Interceptor**: extensão que intercepta chamadas `serviço/método` (hooks).
- **Capability**: permissão declarada pelo pacote e concedida pelo usuário.
- **Perfil**: instância isolada de Atom (pacotes, lockfile, config, dados, secrets, grants).
- **Distribution**: pacote `kind="distribution"` que é um template de perfil.
- **Source**: uma loja configurada no perfil.
- **Store local**: diretório imutável endereçado por conteúdo com pacotes extraídos.
- **Cliente**: processo conectado ao bus via socket/WS (UI, script), não iniciado como provider.
- **Mod (`ui-mod`)**: pacote de UI web de qualquer tamanho (botão, texto, painel, tela). Carregado pelo shell `ui-web` (ADR-016).
- **Ponto de contribuição**: lugar semântico (ex.: `ui.menu.items`) definido por um contrato de UI, onde mods contribuem itens e um mod renderer os apresenta.
- **ctx**: contexto `{tenant,user,profile,session,trace}` de toda mensagem.
