# `@atom/sdk`

SDK TypeScript mínimo para extensões `atom/1`. Zero dependências, Node 18+.

Cobre: handshake (`atom.initialize`), `atom.call`, streams
(`atom.stream.*`), interceptors (`interceptor.invoke` + `atom.proceed`) e
eventos (`event.deliver`, `kind.load/unload`). Todo o resto (storage,
secrets, registry) usa `client.request(method, params)` genérico.

## Uso

```ts
import { run } from "@atom/sdk";

await run(
  {
    provides: [{ contract: "ai.tools@0", version: "0.1.0" }],
    subscribes: ["atom.config.changed"],
  },
  {
    async onInvoke(client, inv) {
      if (inv.service === "ai.tools@0" && inv.method === "list") return [];
      throw new RpcError(-32601, "desconhecido");
    },
    async onEvent(client, ev) {
      client.log("info", `evento ${ev.topic}`);
    },
  },
);
```

Protocolo: stdin/stdout NDJSON, uma mensagem JSON por linha (spec
`docs/specs/protocol.md` §1). Ver `examples/echo.ts`.
