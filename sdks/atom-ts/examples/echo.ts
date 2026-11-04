// Exemplo mínimo: espelha `testdata/echo` em TypeScript.
import { run } from "../src/index.ts";

await run(
  { provides: [{ contract: "test.echo@0", version: "0.1.0" }] },
  {
    async onInvoke(_client, inv) {
      return inv.params;
    },
  },
);
