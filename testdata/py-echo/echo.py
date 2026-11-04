import json
import sys

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception as e:
        print(f"linha inválida: {e}", file=sys.stderr)
        continue
    method = msg.get("method", "")
    mid = msg.get("id")

    def send(v):
        sys.stdout.write(json.dumps(v) + "\n")
        sys.stdout.flush()

    if method == "atom.initialize":
        if mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "result": {"protocol": "atom/1", "provides": [{"contract": "test.pyecho@0", "version": "0.1.0"}]}})
    elif method == "atom.shutdown":
        if mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "atom.exit":
        break
    elif method == "service.invoke":
        p = msg.get("params", {})
        m = p.get("method", "")
        if m == "echo" and mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "result": p.get("params", {})})
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "desconhecido"}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "desconhecido"}})
