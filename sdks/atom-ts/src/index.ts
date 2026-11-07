/**
 * SDK TypeScript mínimo para extensões `atom/1`.
 *
 * Cobre o essencial da v0.9: handshake (`atom.initialize`), `atom.call`,
 * streams (`atom.stream.*`), interceptors (`interceptor.invoke` +
 * `atom.proceed`) e eventos (`atom.events.*`, `event.deliver`, `kind.load`).
 * Zero dependências — só builtins do Node 18+.
 *
 * Uso:
 * ```ts
 * import { run } from "@atom/sdk";
 * run({
 *   provides: [{ contract: "test.echo@0", version: "0.1.0" }],
 *   async onInvoke(inv) { return inv.params; },
 * });
 * ```
 */

export interface ProvidesEntry {
  contract: string;
  version: string;
}

export interface InterceptDecl {
  match: string;
  phase: string;
  priority?: number;
}

export interface Declare {
  provides?: ProvidesEntry[];
  handles?: string[];
  intercepts?: InterceptDecl[];
  subscribes?: string[];
  /** (v1.1) `"credit"` = produtor quer `atom.stream.credit` (`[P-STREAM-3]`). */
  flow?: string;
}

export interface Ctx {
  tenant: string;
  user: string;
  profile: string;
  session?: string;
  trace?: string;
  caller?: string;
  /** (v1.1) Prazo absoluto da cadeia em epoch ms (`[P-DL-1]`). */
  deadline?: number;
}

export interface Invoke {
  service: string;
  method: string;
  params: unknown;
  stream: string | null;
  ctx: Ctx;
  /** Sinal de cancelamento deste request (`$/cancelRequest`, `[P-CANCEL-1]`). */
  signal: AbortSignal;
}

export interface Intercept {
  phase: string;
  service: string;
  method: string;
  params: unknown;
  result?: unknown;
  error?: unknown;
  /** Token p/ `client.proceed` (só em `around`). */
  proceedToken: string | null;
  ctx: Ctx;
  /** Sinal de cancelamento deste request (`$/cancelRequest`, `[P-CANCEL-1]`). */
  signal: AbortSignal;
}

export type InterceptAction =
  | { action: "continue" }
  | { action: "modify"; params?: unknown; result?: unknown }
  | { action: "respond"; result: unknown }
  | { action: "reject"; error: { code: number; message: string } };

export interface EventMsg {
  topic: string;
  data: unknown;
  seq: number;
  /** (v1.1) Instância do log de eventos (`[P-EV-1]`); ausente = evento v1.0. */
  epoch?: string;
  /** (v1.1) Offset global ao perfil; com `from`, at-least-once: deduplique por `(epoch, offset)` (`[P-EV-4]`). */
  offset?: number;
  ctx: Ctx;
}

/** Cursor de retomada de eventos (spec §7, `[P-EV-3]`). */
export interface EventCursor {
  epoch: string;
  offset: number;
}

/** Lacuna na entrega de eventos (`atom.events.gap`, `[P-EV-3]`/`[P-EV-5]`). */
export interface EventsGap {
  subscription: string;
  epoch: string;
  from?: number;
  to?: number;
}

/** Mudança de KV (`storage.changed`, spec §9.1 `[P-KV-6]`). */
export interface StorageChanged {
  subscription: string;
  key?: string;
  version?: string;
  deleted?: boolean;
  /** `true` = fila cheia, mudanças descartadas: releia o que interessa. */
  overflow?: boolean;
}

/** Crédito de emissão (`atom.stream.credit`, `[P-STREAM-3]`). */
export interface StreamCredit {
  stream: string;
  until: number;
}

export interface KindPackage {
  manifest: unknown;
  dir: string;
  config: unknown;
}

export interface StreamChunk {
  stream: string;
  seq: number;
  data: unknown;
}

export interface StreamEnd {
  stream: string;
  result?: unknown;
  error?: { code: number; message: string };
}

export interface RpcErrorShape {
  code: number;
  message: string;
  data?: unknown;
}

export class RpcError extends Error {
  code: number;
  data?: unknown;
  constructor(code: number, message: string, data?: unknown) {
    super(`${message} (${code})`);
    this.code = code;
    this.data = data;
  }
  toJSON(): RpcErrorShape {
    return { code: this.code, message: this.message, data: this.data };
  }
}

const METHOD_NOT_FOUND = -32601;

function strField(v: Record<string, unknown>, key: string): string {
  const x = v[key];
  return typeof x === "string" ? x : "";
}

function parseCtx(p: Record<string, unknown>): Ctx {
  const c = p["$ctx"];
  if (c !== null && typeof c === "object") {
    const o = c as Record<string, unknown>;
    return {
      tenant: typeof o["tenant"] === "string" ? (o["tenant"] as string) : "local",
      user: typeof o["user"] === "string" ? (o["user"] as string) : "local",
      profile: typeof o["profile"] === "string" ? (o["profile"] as string) : "",
      session: typeof o["session"] === "string" ? (o["session"] as string) : undefined,
      trace: typeof o["trace"] === "string" ? (o["trace"] as string) : undefined,
      caller: typeof o["caller"] === "string" ? (o["caller"] as string) : undefined,
      deadline: typeof o["deadline"] === "number" ? (o["deadline"] as number) : undefined,
    };
  }
  return { tenant: "local", user: "local", profile: "" };
}

/** Canal de saída p/ o core. */
export class Client {
  private write: (msg: unknown) => void;
  private pending = new Map<
    string,
    { resolve: (v: unknown) => void; reject: (e: RpcError) => void }
  >();
  private next = 1;

  /** @internal */
  constructor(write: (msg: unknown) => void) {
    this.write = write;
  }

  /** @internal Resolve resposta a um request nosso. Retorna true se era nossa. */
  resolveResponse(msg: Record<string, unknown>): boolean {
    if ("method" in msg) return false;
    const id = msg["id"];
    if (typeof id !== "string") return false;
    const p = this.pending.get(id);
    if (!p) return false;
    this.pending.delete(id);
    if ("error" in msg && msg["error"] != null) {
      const e = msg["error"] as Record<string, unknown>;
      p.reject(
        new RpcError(
          typeof e["code"] === "number" ? e["code"] : -32603,
          typeof e["message"] === "string" ? e["message"] : "erro do core",
          e["data"],
        ),
      );
    } else {
      p.resolve(msg["result"] ?? null);
    }
    return true;
  }

  /** Request genérico ao core (com resposta). */
  request(method: string, params: unknown): Promise<unknown> {
    const id = `sdk-${this.next++}`;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      try {
        this.write({ jsonrpc: "2.0", id, method, params });
      } catch {
        this.pending.delete(id);
        reject(new RpcError(-32603, "core desligado"));
      }
    });
  }

  /** Notificação ao core (sem resposta). */
  notify(method: string, params: unknown): void {
    this.write({ jsonrpc: "2.0", method, params });
  }

  /**
   * `atom.call` (spec protocol §5.1). Propaga `$ctx.deadline` do `Ctx` em
   * tratamento: passe `inv.ctx`/`it.ctx` (spec §3, `[P-DL-1]`).
   * Sem deadline no contexto, sem prazo (v1.0).
   */
  // spec: [P-DL-1]
  call(
    service: string,
    method: string,
    params: unknown,
    target?: string,
    stream = false,
    ctx?: Ctx,
  ): Promise<unknown> {
    const p: Record<string, unknown> = { service, method, params, stream };
    if (target !== undefined) p["target"] = target;
    if (ctx?.deadline !== undefined) p["$ctx"] = { deadline: ctx.deadline };
    return this.request("atom.call", p);
  }

  /**
   * `atom.call` com janela de backpressure opt-in (v1.1, `[P-STREAM-2]`):
   * `1 <= window <= 4096`. Confirme com `streamAck`.
   */
  // spec: [P-STREAM-2]
  // spec: [P-DL-1]
  callWithWindow(
    service: string,
    method: string,
    params: unknown,
    target: string | undefined,
    stream: boolean,
    window: number,
    ctx?: Ctx,
  ): Promise<unknown> {
    const p: Record<string, unknown> = { service, method, params, stream };
    if (target !== undefined) p["target"] = target;
    p["flow"] = { window };
    if (ctx?.deadline !== undefined) p["$ctx"] = { deadline: ctx.deadline };
    return this.request("atom.call", p);
  }

  /** `atom.proceed` p/ interceptors `around` (uso único). Propaga `deadline` de `ctx` (`[P-DL-1]`). */
  // spec: [P-DL-1]
  proceed(token: string, ctx?: Ctx): Promise<unknown> {
    const p: Record<string, unknown> = { token };
    if (ctx?.deadline !== undefined) p["$ctx"] = { deadline: ctx.deadline };
    return this.request("atom.proceed", p);
  }

  /** `atom.events.publish`. */
  publish(topic: string, data: unknown): Promise<unknown> {
    return this.request("atom.events.publish", { topic, data });
  }

  /** `atom.events.subscribe` → id da assinatura. */
  subscribe(topics: string[]): Promise<unknown> {
    return this.request("atom.events.subscribe", { topics });
  }

  /**
   * `atom.events.subscribe` com retomada (v1.1, `[P-EV-3]`): reentrega
   * retidos com `offset > from.offset`, depois ao vivo. Gap antes quando o
   * `epoch` diverge ou o offset saiu do ring.
   */
  // spec: [P-EV-3]
  subscribeFrom(topics: string[], from: EventCursor): Promise<unknown> {
    return this.request("atom.events.subscribe", { topics, from });
  }

  /** `atom.events.unsubscribe` (spec §7). */
  unsubscribe(topics: string[]): Promise<unknown> {
    return this.request("atom.events.unsubscribe", { topics });
  }

  /** `atom.storage.watch` (v1.1, `[P-KV-6]`) → `{subscription}`. */
  // spec: [P-KV-6]
  storageWatch(prefix: string): Promise<unknown> {
    return this.request("atom.storage.watch", { prefix });
  }

  /** `atom.storage.unwatch` (v1.1, `[P-KV-6]`). */
  // spec: [P-KV-6]
  storageUnwatch(subscription: string): Promise<unknown> {
    return this.request("atom.storage.unwatch", { subscription });
  }

  /** `atom.log` (notificação). */
  log(level: string, msg: string): void {
    this.notify("atom.log", { level, msg });
  }

  streamChunk(stream: string, seq: number, data: unknown): void {
    this.notify("atom.stream.chunk", { stream, seq, data });
  }

  streamEnd(stream: string, result: unknown): void {
    this.notify("atom.stream.end", { stream, result });
  }

  streamError(stream: string, error: RpcError | RpcErrorShape): void {
    const e = error instanceof RpcError ? error.toJSON() : error;
    this.notify("atom.stream.end", { stream, error: e });
  }

  /**
   * `atom.stream.ack` cumulativo (v1.1, `[P-STREAM-2]`, lado chamador).
   * Sem `flow` no setup, o core ignora.
   */
  // spec: [P-STREAM-2]
  streamAck(stream: string, seq: number): void {
    this.notify("atom.stream.ack", { stream, seq });
  }

  streamCancel(stream: string): void {
    this.notify("atom.stream.cancel", { stream });
  }
}

export interface Handler {
  onInvoke?: (client: Client, inv: Invoke) => Promise<unknown>;
  onIntercept?: (client: Client, it: Intercept) => Promise<InterceptAction>;
  onEvent?: (client: Client, ev: EventMsg) => Promise<void> | void;
  onKindLoad?: (client: Client, pkg: KindPackage) => Promise<void> | void;
  onKindUnload?: (client: Client, pkg: string) => Promise<void> | void;
  onStreamChunk?: (client: Client, chunk: StreamChunk) => Promise<void> | void;
  onStreamEnd?: (client: Client, end: StreamEnd) => Promise<void> | void;
  onStreamCancel?: (client: Client, stream: string) => Promise<void> | void;
  /** Crédito de emissão (`atom.stream.credit`, `[P-STREAM-3]`). */
  onStreamCredit?: (client: Client, credit: StreamCredit) => Promise<void> | void;
  /** Lacuna de eventos (`atom.events.gap`, `[P-EV-3]`/`[P-EV-5]`). */
  onEventsGap?: (client: Client, gap: EventsGap) => Promise<void> | void;
  /** Mudança de KV (`storage.changed`, `[P-KV-6]`). */
  onStorageChanged?: (client: Client, change: StorageChanged) => Promise<void> | void;
  onShutdown?: (client: Client) => Promise<void> | void;
}

function ok(id: unknown, result: unknown): Record<string, unknown> {
  return { jsonrpc: "2.0", id, result };
}

function err(id: unknown, code: number, message: string): Record<string, unknown> {
  return { jsonrpc: "2.0", id, error: { code, message } };
}

/** Normaliza id JSON-RPC para chave do mapa de cancelamento (`[P-CANCEL-1]`). */
export function cancelKey(id: unknown): string {
  return JSON.stringify(id) ?? "null";
}

/** Mensagem escrita pelo dispatch (`run`); `exit: true` = `atom.exit`. */
export interface DispatchOut {
  msg?: Record<string, unknown>;
  exit?: boolean;
}

/**
 * Núcleo testável do dispatch (mesma lógica do `run`, sem stdin/stdout).
 * `cancels` mapeia id (normalizado) → controller; `$/cancelRequest`
 * (notificação ou request — request responde `null`) aborta o controller;
 * cleanup após assentamento do handler. `onStreamCancel` e resto intocados.
 */
// spec: [P-CANCEL-1]
export async function dispatchMessage(
  client: Client,
  declare: Declare,
  handler: Handler,
  cancels: Map<string, AbortController>,
  write: (msg: unknown) => void,
  m: Record<string, unknown>,
): Promise<DispatchOut | null> {
  if (client.resolveResponse(m)) return null;
  if (!("method" in m)) return null;
  const method = typeof m["method"] === "string" ? (m["method"] as string) : "";
  const id = ("id" in m ? m["id"] : undefined) as unknown;
  const hasId = "id" in m;
  const params = (
    m["params"] !== null && typeof m["params"] === "object" ? m["params"] : {}
  ) as Record<string, unknown>;
  switch (method) {
    case "atom.initialize": {
      const provides = (declare.provides ?? []).map((p) => ({
        contract: p.contract,
        version: p.version,
      }));
      const intercepts = (declare.intercepts ?? []).map((i) => ({
        match: i.match,
        phase: i.phase,
        priority: i.priority ?? 100,
      }));
      if (hasId) {
        const result: Record<string, unknown> = {
          protocol: "atom/1",
          provides,
          handles: declare.handles ?? [],
          intercepts,
          subscribes: declare.subscribes ?? [],
        };
        if (declare.flow !== undefined) result["flow"] = declare.flow;
        write(ok(id, result));
      }
      return null;
    }
    case "atom.shutdown":
      await handler.onShutdown?.(client);
      if (hasId) write(ok(id, {}));
      return null;
    case "atom.exit":
      return { exit: true };
    case "service.invoke":
    case "interceptor.invoke": {
      if (!hasId) {
        // Sem id: comportamento v1.0.
        if (method === "interceptor.invoke") {
          const ctl = new AbortController();
          const it: Intercept = {
            phase: strField(params, "phase"),
            service: strField(params, "service"),
            method: strField(params, "method"),
            params: params["params"] ?? null,
            result: params["result"],
            error: params["error"],
            proceedToken:
              typeof params["$proceed"] === "string" ? (params["$proceed"] as string) : null,
            ctx: parseCtx(params),
            signal: ctl.signal,
          };
          void handler.onIntercept?.(client, it);
        }
        return null;
      }
      const key = cancelKey(id);
      const ctl = new AbortController();
      cancels.set(key, ctl);
      const done = (): void => {
        if (cancels.get(key) === ctl) cancels.delete(key);
      };
      if (method === "service.invoke") {
        const inv: Invoke = {
          service: strField(params, "service"),
          method: strField(params, "method"),
          params: params["params"] ?? null,
          stream: typeof params["stream"] === "string" ? (params["stream"] as string) : null,
          ctx: parseCtx(params),
          signal: ctl.signal,
        };
        void (async () => {
          try {
            const r = handler.onInvoke ? await handler.onInvoke(client, inv) : null;
            if (handler.onInvoke) write(ok(id, r ?? null));
            else write(err(id, METHOD_NOT_FOUND, `método ${inv.service}#${inv.method} desconhecido`));
          } catch (e) {
            if (e instanceof RpcError) write({ jsonrpc: "2.0", id, error: e.toJSON() });
            else write(err(id, -32603, e instanceof Error ? e.message : String(e)));
          } finally {
            done();
          }
        })();
      } else {
        const it: Intercept = {
          phase: strField(params, "phase"),
          service: strField(params, "service"),
          method: strField(params, "method"),
          params: params["params"] ?? null,
          result: params["result"],
          error: params["error"],
          proceedToken:
            typeof params["$proceed"] === "string" ? (params["$proceed"] as string) : null,
          ctx: parseCtx(params),
          signal: ctl.signal,
        };
        void (async () => {
          try {
            const action = handler.onIntercept
              ? await handler.onIntercept(client, it)
              : { action: "continue" as const };
            write(ok(id, action));
          } finally {
            done();
          }
        })();
      }
      return null;
    }
    case "$/cancelRequest": {
      // `[P-CANCEL-1]`: `params.id` conhecido → aborta; senão ignora.
      const target = (params as Record<string, unknown>)["id"];
      if (target !== undefined) {
        const ctl = cancels.get(cancelKey(target));
        if (ctl) ctl.abort();
      }
      if (hasId) write(ok(id, null));
      return null;
    }
    case "event.deliver":
      void handler.onEvent?.(client, {
        topic: strField(params, "topic"),
        data: params["data"] ?? null,
        seq: typeof params["seq"] === "number" ? (params["seq"] as number) : 0,
        epoch: typeof params["epoch"] === "string" ? (params["epoch"] as string) : undefined,
        offset: typeof params["offset"] === "number" ? (params["offset"] as number) : undefined,
        ctx: parseCtx(params),
      });
      return null;
    case "kind.load":
      void (async () => {
        try {
          await handler.onKindLoad?.(client, {
            manifest: params["package"] ?? null,
            dir: strField(params, "dir"),
            config: params["config"] ?? null,
          });
          if (hasId) write(ok(id, {}));
        } catch (e) {
          if (hasId) write(err(id, -32030, e instanceof Error ? e.message : String(e)));
        }
      })();
      return null;
    case "kind.unload": {
      const name = typeof params["package"] === "string" ? (params["package"] as string) : "";
      void (async () => {
        await handler.onKindUnload?.(client, name);
        if (hasId) write(ok(id, {}));
      })();
      return null;
    }
    case "atom.stream.chunk":
      void handler.onStreamChunk?.(client, {
        stream: strField(params, "stream"),
        seq: typeof params["seq"] === "number" ? (params["seq"] as number) : 0,
        data: params["data"] ?? null,
      });
      return null;
    case "atom.stream.end": {
      const e = params["error"] as Record<string, unknown> | undefined;
      void handler.onStreamEnd?.(client, {
        stream: strField(params, "stream"),
        result: params["result"],
        error:
          e != null
            ? {
                code: typeof e["code"] === "number" ? (e["code"] as number) : -32603,
                message: typeof e["message"] === "string" ? (e["message"] as string) : "erro",
              }
            : undefined,
      });
      return null;
    }
    case "atom.stream.cancel":
      void handler.onStreamCancel?.(client, strField(params, "stream"));
      return null;
    case "atom.stream.credit":
      void handler.onStreamCredit?.(client, {
        stream: strField(params, "stream"),
        until: typeof params["until"] === "number" ? (params["until"] as number) : 0,
      });
      return null;
    case "atom.events.gap":
      void handler.onEventsGap?.(client, {
        subscription: strField(params, "subscription"),
        epoch: strField(params, "epoch"),
        from: typeof params["from"] === "number" ? (params["from"] as number) : undefined,
        to: typeof params["to"] === "number" ? (params["to"] as number) : undefined,
      });
      return null;
    case "storage.changed":
      void handler.onStorageChanged?.(client, {
        subscription: strField(params, "subscription"),
        key: typeof params["key"] === "string" ? (params["key"] as string) : undefined,
        version: typeof params["version"] === "string" ? (params["version"] as string) : undefined,
        deleted: typeof params["deleted"] === "boolean" ? (params["deleted"] as boolean) : undefined,
        overflow: typeof params["overflow"] === "boolean" ? (params["overflow"] as boolean) : undefined,
      });
      return null;
    default:
      if (hasId) write(err(id, METHOD_NOT_FOUND, `método ${method} desconhecido`));
      return null;
  }
}

export async function run(declare: Declare, handler: Handler): Promise<void> {
  const write = (msg: unknown): void => {
    process.stdout.write(JSON.stringify(msg) + "\n");
  };
  const client = new Client(write);
  const cancels = new Map<string, AbortController>();

  let buf = "";
  const onData = async (chunk: Buffer | string): Promise<void> => {
    buf += chunk.toString("utf8");
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) {
      if (line.trim() === "") continue;
      let msg: unknown;
      try {
        msg = JSON.parse(line);
      } catch {
        process.stderr.write(`linha inválida: ${line.slice(0, 80)}\n`);
        continue;
      }
      if (msg === null || typeof msg !== "object") continue;
      const out = await dispatchMessage(
        client,
        declare,
        handler,
        cancels,
        write,
        msg as Record<string, unknown>,
      );
      if (out?.exit) process.exit(0);
    }
  };

  process.stdin.on("data", (c: Buffer) => {
    void onData(c);
  });
  await new Promise<never>(() => {});
}
