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
}

export interface Ctx {
  tenant: string;
  user: string;
  profile: string;
  session?: string;
  trace?: string;
  caller?: string;
}

export interface Invoke {
  service: string;
  method: string;
  params: unknown;
  stream: string | null;
  ctx: Ctx;
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
  ctx: Ctx;
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

  /** `atom.call` (spec protocol §5.1). */
  call(
    service: string,
    method: string,
    params: unknown,
    target?: string,
    stream = false,
  ): Promise<unknown> {
    const p: Record<string, unknown> = { service, method, params, stream };
    if (target !== undefined) p["target"] = target;
    return this.request("atom.call", p);
  }

  /** `atom.proceed` p/ interceptors `around` (uso único). */
  proceed(token: string): Promise<unknown> {
    return this.request("atom.proceed", { token });
  }

  /** `atom.events.publish`. */
  publish(topic: string, data: unknown): Promise<unknown> {
    return this.request("atom.events.publish", { topic, data });
  }

  /** `atom.events.subscribe` → id da assinatura. */
  subscribe(topics: string[]): Promise<unknown> {
    return this.request("atom.events.subscribe", { topics });
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
  onShutdown?: (client: Client) => Promise<void> | void;
}

function ok(id: unknown, result: unknown): Record<string, unknown> {
  return { jsonrpc: "2.0", id, result };
}

function err(id: unknown, code: number, message: string): Record<string, unknown> {
  return { jsonrpc: "2.0", id, error: { code, message } };
}

/**
 * Loop principal: handshake + dispatch NDJSON até `atom.exit`.
 * Lê stdin, escreve stdout. Nunca retorna em uso normal.
 */
export async function run(declare: Declare, handler: Handler): Promise<void> {
  const write = (msg: unknown): void => {
    process.stdout.write(JSON.stringify(msg) + "\n");
  };
  const client = new Client(write);

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
      const m = msg as Record<string, unknown>;
      if (client.resolveResponse(m)) continue;
      if (!("method" in m)) continue;
      const method = typeof m["method"] === "string" ? (m["method"] as string) : "";
      const id = ("id" in m ? m["id"] : undefined) as unknown;
      const hasId = "id" in m;
      const params = (
        m["params"] !== null && typeof m["params"] === "object"
          ? m["params"]
          : {}
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
            write(
              ok(id, {
                protocol: "atom/1",
                provides,
                handles: declare.handles ?? [],
                intercepts,
                subscribes: declare.subscribes ?? [],
              }),
            );
          }
          break;
        }
        case "atom.shutdown":
          await handler.onShutdown?.(client);
          if (hasId) write(ok(id, {}));
          break;
        case "atom.exit":
          process.exit(0);
          break;
        case "service.invoke": {
          const inv: Invoke = {
            service: strField(params, "service"),
            method: strField(params, "method"),
            params: params["params"] ?? null,
            stream:
              typeof params["stream"] === "string"
                ? (params["stream"] as string)
                : null,
            ctx: parseCtx(params),
          };
          void (async () => {
            try {
              const r = handler.onInvoke
                ? await handler.onInvoke(client, inv)
                : null;
              if (hasId) {
                if (handler.onInvoke) write(ok(id, r ?? null));
                else write(err(id, METHOD_NOT_FOUND, `método ${inv.service}#${inv.method} desconhecido`));
              }
            } catch (e) {
              if (hasId) {
                if (e instanceof RpcError) write({ jsonrpc: "2.0", id, error: e.toJSON() });
                else write(err(id, -32603, e instanceof Error ? e.message : String(e)));
              }
            }
          })();
          break;
        }
        case "interceptor.invoke": {
          const it: Intercept = {
            phase: strField(params, "phase"),
            service: strField(params, "service"),
            method: strField(params, "method"),
            params: params["params"] ?? null,
            result: params["result"],
            error: params["error"],
            proceedToken:
              typeof params["$proceed"] === "string"
                ? (params["$proceed"] as string)
                : null,
            ctx: parseCtx(params),
          };
          void (async () => {
            const action = handler.onIntercept
              ? await handler.onIntercept(client, it)
              : { action: "continue" as const };
            if (hasId) write(ok(id, action));
          })();
          break;
        }
        case "event.deliver":
          void handler.onEvent?.(client, {
            topic: strField(params, "topic"),
            data: params["data"] ?? null,
            seq: typeof params["seq"] === "number" ? (params["seq"] as number) : 0,
            ctx: parseCtx(params),
          });
          break;
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
              if (hasId) {
                write(err(id, -32030, e instanceof Error ? e.message : String(e)));
              }
            }
          })();
          break;
        case "kind.unload": {
          const name =
            typeof params["package"] === "string" ? (params["package"] as string) : "";
          void (async () => {
            await handler.onKindUnload?.(client, name);
            if (hasId) write(ok(id, {}));
          })();
          break;
        }
        case "atom.stream.chunk":
          void handler.onStreamChunk?.(client, {
            stream: strField(params, "stream"),
            seq: typeof params["seq"] === "number" ? (params["seq"] as number) : 0,
            data: params["data"] ?? null,
          });
          break;
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
          break;
        }
        case "atom.stream.cancel":
          void handler.onStreamCancel?.(client, strField(params, "stream"));
          break;
        default:
          if (hasId) write(err(id, METHOD_NOT_FOUND, `método ${method} desconhecido`));
          break;
      }
    }
  };

  process.stdin.on("data", (c: Buffer) => {
    void onData(c);
  });
  await new Promise<never>(() => {});
}
