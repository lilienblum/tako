#!/usr/bin/env python3

import json
import os
import socket


SOCK_PATH = "/var/run/tako/tako.sock"
ROUTES_PATH = "/opt/tako/routes.json"
LAST_DEPLOY_PATH = "/opt/tako/last_deploy.json"


def load_routes():
    if not os.path.exists(ROUTES_PATH):
        return []
    try:
        with open(ROUTES_PATH, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return []


def handle_command(cmd):
    if cmd.get("command") == "routes":
        # Expected shape: [{"app": "...", "routes": ["..."]}]
        return {"status": "ok", "data": {"routes": load_routes()}}

    if cmd.get("command") == "deploy":
        try:
            with open(LAST_DEPLOY_PATH, "w", encoding="utf-8") as f:
                json.dump(cmd, f)
        except Exception:
            pass
        return {"status": "ok", "data": {"ok": True}}

    return {"status": "ok", "data": {}}


def main():
    os.makedirs("/var/run/tako", exist_ok=True)
    os.makedirs("/opt/tako", exist_ok=True)

    try:
        os.unlink(SOCK_PATH)
    except FileNotFoundError:
        pass

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(SOCK_PATH)
    s.listen(64)

    while True:
        conn, _ = s.accept()
        try:
            data = conn.recv(1024 * 1024)
            raw = data.decode("utf-8", errors="ignore").strip()
            # nc may include trailing newlines.
            raw = raw.strip()
            cmd = json.loads(raw) if raw else {}
            resp = handle_command(cmd)
        except Exception as e:
            resp = {"status": "error", "message": str(e)}

        try:
            conn.sendall((json.dumps(resp) + "\n").encode("utf-8"))
        except Exception:
            pass
        try:
            conn.close()
        except Exception:
            pass


if __name__ == "__main__":
    main()
