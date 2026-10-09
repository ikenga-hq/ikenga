#!/usr/bin/env python3
"""A FAKE PostgreSQL server for the port-lock squatting test (test-portlock-*.sh).

It is what a local account that grabbed a tunnel's port while the tunnel was
down could run. It listens on one address:port, speaks just enough of the
PostgreSQL v3 wire protocol to get a real libpq client to authenticate, and
LOGS what the client did, so the test can prove from the server's side whether
the password was handed over:

  mode cleartext   asks for a cleartext password (AuthenticationCleartextPassword)
  mode md5         asks for an md5 password
  mode trust       says AuthenticationOk at once, no authentication at all, then
                   ReadyForQuery (a server feeding the backup job forged data)

Log lines (one per event, appended to --log):
  startup user=<u> database=<d>
  auth-requested <mode>
  PASSWORD-RECEIVED <value>        the client sent a password message
  client-closed-without-password   the client hung up instead (require_auth)
  auth-ok-sent
  query-received

The password values in these tests are FAKE canaries; the real one never goes
near this file.
"""
import argparse
import socket
import struct
import sys


def log(path, line):
    with open(path, "a") as f:
        f.write(line + "\n")


def recv_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def read_startup(conn):
    """Return the parameters of the StartupMessage, answering SSL/GSS requests with 'N'."""
    while True:
        head = recv_exact(conn, 8)
        if head is None:
            return None
        length, code = struct.unpack("!II", head)
        if code in (80877103, 80877104):  # SSLRequest, GSSENCRequest: not supported
            conn.sendall(b"N")
            continue
        body = recv_exact(conn, length - 8) or b""
        parts = body.split(b"\x00")
        params = {}
        for i in range(0, len(parts) - 1, 2):
            if parts[i]:
                params[parts[i].decode()] = parts[i + 1].decode()
        return params


def message(kind, payload=b""):
    return kind + struct.pack("!I", len(payload) + 4) + payload


def serve(conn, mode, logpath):
    conn.settimeout(10)
    try:
        params = read_startup(conn)
        if params is None:
            return
        log(logpath, "startup user=%s database=%s" % (params.get("user"), params.get("database")))
        if mode == "trust":
            conn.sendall(message(b"R", struct.pack("!I", 0)))
            log(logpath, "auth-ok-sent")
            conn.sendall(message(b"S", b"server_version\x0016.0\x00"))
            conn.sendall(message(b"Z", b"I"))
            data = conn.recv(4096)
            if data:
                log(logpath, "query-received")
            return
        if mode == "cleartext":
            conn.sendall(message(b"R", struct.pack("!I", 3)))
        else:  # md5
            conn.sendall(message(b"R", struct.pack("!I", 5) + b"salt"))
        log(logpath, "auth-requested " + mode)
        head = recv_exact(conn, 5)
        if head is None:
            log(logpath, "client-closed-without-password")
            return
        kind, length = head[:1], struct.unpack("!I", head[1:])[0]
        body = recv_exact(conn, length - 4) or b""
        if kind == b"p":
            log(logpath, "PASSWORD-RECEIVED " + body.rstrip(b"\x00").decode(errors="replace"))
        conn.sendall(message(b"E", b"SFATAL\x00C28P01\x00Mpassword authentication failed\x00\x00"))
    except (socket.timeout, ConnectionError, OSError):
        pass
    finally:
        try:
            conn.close()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bind", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--mode", choices=["cleartext", "md5", "trust"], required=True)
    ap.add_argument("--log", required=True)
    a = ap.parse_args()
    fam = socket.AF_INET6 if ":" in a.bind else socket.AF_INET
    srv = socket.socket(fam, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((a.bind, a.port))
    srv.listen(8)
    print("listening", flush=True)
    while True:
        conn, _ = srv.accept()
        serve(conn, a.mode, a.log)


if __name__ == "__main__":
    sys.exit(main())
