import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { Client, dispatchMessage, type Declare, type Handler } from "../src/index.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const schemasDir = join(root, "docs", "specs", "schemas", "atom-1");

function schema(name: string): { examples: Record<string, unknown>[] } {
  const text = readFileSync(join(schemasDir, name), "utf8");
  const raw = JSON.parse(text) as Record<string, unknown>;
  const examples = (raw["examples"] as Record<string, unknown>[] | undefined) ?? [];
  assert.ok(examples.length > 0, `${name} sem examples ([CF-3])`);
  return { examples };
}

function paramsOf(name: string, i: number): Record<string, unknown> {
  const { examples } = schema(name);
  assert.ok(i < examples.length, `${name} exemplo ${i} ausente`);
  return examples[i]["params"] as Record<string, unknown>;
}

function fakeClient(): { client: Client; seen: Record<string, unknown>[] } {
  const seen: Record<string, unknown>[] = [];
  const client = new Client((m) => {
    seen.push(m as Record<string, unknown>);
  });
  return { client, seen };
}

/** Completa o último request com `result` e devolve o valor resolvido. */
async function answerLast(
  client: Client,
  seen: Record<string, unknown>[],
  result: unknown,
): Promise<unknown> {
  const msg = seen[seen.length - 1];
  assert.equal(msg["method"], seen[seen.length - 1]["method"]);
  const ok = client.resolveResponse({ id: msg["id"], result });
  assert.equal(ok, true);
  return result;
}

// spec: [CF-3]
// spec: [P-DL-1]
// spec: [P-STREAM-2]
test("sdk-ts: deadline propaga, window opt-in", async () => {
  const { client, seen } = fakeClient();
  const p0 = paramsOf("atom.call.json", 0);
  const pending0 = client.call(p0["service"] as string, p0["method"] as string, p0["params"]);
  assert.equal(seen[seen.length - 1]["method"], "atom.call");
  assert.deepEqual(
    (seen[seen.length - 1]["params"] as Record<string, unknown>)["$ctx"],
    undefined,
  );
  await answerLast(client, seen, { ok: true });
  await pending0;

  const ctx = { tenant: "local", user: "local", profile: "dev", deadline: 1700000000000 };
  const pending1 = client.call("s", "m", {}, undefined, false, ctx);
  assert.deepEqual(
    ((seen[seen.length - 1]["params"] as Record<string, unknown>)["$ctx"] as Record<
      string,
      unknown
    >)["deadline"],
    1700000000000,
  );
  await answerLast(client, seen, { ok: true });
  await pending1;

  const pending2 = client.callWithWindow("s", "m", {}, undefined, true, 16, ctx);
  const last = seen[seen.length - 1]["params"] as Record<string, unknown>;
  assert.deepEqual(last["flow"], { window: 16 });
  assert.deepEqual((last["$ctx"] as Record<string, unknown>)["deadline"], 1700000000000);
  await answerLast(client, seen, { ok: true });
  await pending2;

  client.streamAck("st_x", 3);
  assert.deepEqual(seen[seen.length - 1], {
    jsonrpc: "2.0",
    method: "atom.stream.ack",
    params: { stream: "st_x", seq: 3 },
  });

  const pending3 = client.proceed("tok-1", ctx);
  assert.deepEqual(
    (
      (seen[seen.length - 1]["params"] as Record<string, unknown>)["$ctx"] as Record<
        string,
        unknown
      >
    )["deadline"],
    1700000000000,
  );
  await answerLast(client, seen, { ok: true });
  await pending3;
});

// spec: [CF-3]
// spec: [P-STREAM-3]
test("sdk-ts: crédito do produtor respeita until", () => {
  const credit = paramsOf("atom.stream.credit.json", 0);
  assert.equal(typeof credit["stream"], "string");
  assert.equal(typeof credit["until"], "number");
  const until = credit["until"] as number;
  const { client, seen } = fakeClient();
  for (let seq = 0; seq <= until; seq++) {
    client.streamChunk(credit["stream"] as string, seq, { seq });
  }
  assert.equal(seen.length, until + 1);
  assert.throws(
    () => {
      const seq = until + 1;
      if (seq > until) throw new Error("excedeu o crédito");
      client.streamChunk(credit["stream"] as string, seq, { seq });
    },
    /crédito/,
  );
});

// spec: [CF-3]
// spec: [P-EV-3]
// spec: [P-KV-6]
test("sdk-ts: subscribe-from, gap e storage.watch", async () => {
  const sub = paramsOf("atom.events.subscribe.json", 0);
  assert.ok("topics" in sub);
  const { client, seen } = fakeClient();
  const pending = client.subscribeFrom(["atom.test"], { epoch: "ep_1", offset: 41 });
  assert.deepEqual(seen[seen.length - 1]["params"], {
    topics: ["atom.test"],
    from: { epoch: "ep_1", offset: 41 },
  });
  await answerLast(client, seen, { subscription: "sub_1" });
  assert.deepEqual((await pending) as Record<string, unknown>, { subscription: "sub_1" });

  const deliver = paramsOf("event.deliver.json", 0);
  assert.ok("topic" in deliver && "seq" in deliver && "$ctx" in deliver);
  const seenKeys = new Set<string>();
  const dedupe = (epoch: string, offset: number): boolean => {
    const k = `${epoch}:${offset}`;
    if (seenKeys.has(k)) return false;
    seenKeys.add(k);
    return true;
  };
  assert.equal(dedupe("ep_1", 42), true);
  assert.equal(dedupe("ep_1", 42), false);
  assert.equal(dedupe("ep_1", 41), true);

  const gap = { subscription: "sub_1", epoch: "ep_1", from: 41, to: 42 };
  assert.equal(gap.from, 41);
  assert.equal(gap.to, 42);

  const pendingW = client.storageWatch("a/");
  assert.deepEqual(seen[seen.length - 1]["params"], { prefix: "a/" });
  await answerLast(client, seen, { subscription: "w_1" });
  await pendingW;

  const pendingU = client.storageUnwatch("w_1");
  assert.deepEqual(seen[seen.length - 1]["params"], { subscription: "w_1" });
  await answerLast(client, seen, {});
  await pendingU;

  for (const change of [
    { subscription: "w_1", key: "a/k", version: "v_1" },
    { subscription: "w_1", key: "a/k", deleted: true },
    { subscription: "w_1", overflow: true },
  ]) {
    assert.equal(change["subscription"], "w_1");
  }
});

// spec: [CF-3]
test("sdk-ts: exemplos dos schemas fazem round-trip", () => {
  for (const f of [
    "atom.call.json",
    "service.invoke.json",
    "interceptor.invoke.json",
    "event.deliver.json",
    "atom.stream.chunk.json",
    "atom.stream.end.json",
    "atom.stream.ack.json",
    "atom.stream.credit.json",
    "atom.events.subscribe.json",
    "ctx.json",
  ]) {
    const { examples } = schema(f);
    for (const ex of examples) {
      const p = ex["params"] as Record<string, unknown>;
      const back = JSON.parse(JSON.stringify(p)) as Record<string, unknown>;
      assert.deepEqual(Object.keys(back).sort(), Object.keys(p).sort(), `${f} round-trip`);
    }
  }
});

// spec: [P-CANCEL-1]
// spec: [P-CANCEL-2]
test("sdk-ts: $/cancelRequest aborta o AbortSignal", async () => {
  const seen: Record<string, unknown>[] = [];
  const client = new Client((m) => {
    seen.push(m as Record<string, unknown>);
  });
  const cancels = new Map<string, AbortController>();
  const declare: Declare = {};
  let observed: AbortSignal | null = null;
  let itObserved: AbortSignal | null = null;
  const handler: Handler = {
    async onInvoke(_c, inv) {
      observed = inv.signal;
      // Prova que o handler observou o cancel disparado (sem depender da
      // cortesia do core): espera o abort, não um sleep.
      await new Promise<void>((resolve, reject) => {
        if (inv.signal.aborted) return resolve();
        const t = setTimeout(() => reject(new Error("cancel não disparou em 2s")), 2000);
        inv.signal.addEventListener("abort", () => {
          clearTimeout(t);
          resolve();
        });
      });
      assert.equal(inv.signal.aborted, true);
      return { ok: true };
    },
    async onIntercept(_c, it) {
      itObserved = it.signal;
      await new Promise<void>((resolve, reject) => {
        if (it.signal.aborted) return resolve();
        const t = setTimeout(() => reject(new Error("cancel do intercept não disparou em 2s")), 2000);
        it.signal.addEventListener("abort", () => {
          clearTimeout(t);
          resolve();
        });
      });
      return { action: "continue" as const };
    },
  };
  const write = (m: unknown): void => {
    seen.push(m as Record<string, unknown>);
  };
  const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 20));
  // `service.invoke` com id numérico: token registrado sob `JSON.stringify`.
  await dispatchMessage(client, declare, handler, cancels, write, {
    jsonrpc: "2.0",
    id: 7,
    method: "service.invoke",
    params: { service: "t@0", method: "slow", params: {} },
  });
  assert.equal(cancels.has("7"), true);
  await dispatchMessage(client, declare, handler, cancels, write, {
    jsonrpc: "2.0",
    method: "$/cancelRequest",
    params: { id: 7 },
  });
  await flush();
  assert.ok(observed !== null && (observed as AbortSignal).aborted);
  assert.equal(seen[seen.length - 1]["id"], 7);
  assert.equal(cancels.has("7"), false);
  // Variante interceptor.invoke (id string).
  await dispatchMessage(client, declare, handler, cancels, write, {
    jsonrpc: "2.0",
    id: "it-1",
    method: "interceptor.invoke",
    params: { phase: "around", service: "t@0", method: "m" },
  });
  assert.equal(cancels.has('"it-1"'), true);
  await dispatchMessage(client, declare, handler, cancels, write, {
    jsonrpc: "2.0",
    method: "$/cancelRequest",
    params: { id: "it-1" },
  });
  await flush();
  assert.ok(itObserved !== null && (itObserved as AbortSignal).aborted);
  // Id desconhecido: ignorado, sem resposta.
  const n = seen.length;
  await dispatchMessage(client, declare, handler, cancels, write, {
    jsonrpc: "2.0",
    method: "$/cancelRequest",
    params: { id: "nope" },
  });
  assert.equal(seen.length, n);
  // `$/cancelRequest` como request-com-id: responde `null`.
  await dispatchMessage(client, declare, handler, cancels, write, {
    jsonrpc: "2.0",
    id: "c1",
    method: "$/cancelRequest",
    params: { id: "nope" },
  });
  assert.equal(seen[seen.length - 1]["id"], "c1");
  assert.equal(seen[seen.length - 1]["result"], null);
});
