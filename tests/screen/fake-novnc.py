# A stand-in for noVNC on port 6080, for tests/screen/linux.sh in CI: what
# the screen view needs of a container, and nothing else. GET of a file
# answers a small page that names noVNC; an upgrade to WebSocket answers 101
# with the right Sec-WebSocket-Accept, then the first bytes a VNC server
# sends, then echoes. It exits after FAKE_SECS (default 90), as a job ends.
# Python's standard library only, so python:alpine runs it as it is.
import base64
import hashlib
import os
import socket
import threading
import time

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
PAGE = b"<!doctype html><title>noVNC</title><p>fake noVNC for tests</p>"


def serve(conn):
    with conn:
        head = b""
        while b"\r\n\r\n" not in head and len(head) < 16384:
            chunk = conn.recv(4096)
            if not chunk:
                return
            head += chunk
        lines = head.split(b"\r\n\r\n", 1)[0].decode("latin-1").split("\r\n")
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            headers[name.strip().lower()] = value.strip()
        if headers.get("upgrade", "").lower() == "websocket":
            key = headers.get("sec-websocket-key", "")
            accept = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
            conn.sendall(
                (
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                    f"Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n"
                    "Sec-WebSocket-Protocol: binary\r\n\r\n"
                ).encode()
                + b"RFB 003.008\n"
            )
            while True:
                data = conn.recv(4096)
                if not data:
                    return
                conn.sendall(data)
        conn.sendall(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: "
            + str(len(PAGE)).encode()
            + b"\r\nConnection: close\r\n\r\n"
            + PAGE
        )


def main():
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("0.0.0.0", 6080))
    listener.listen(16)
    listener.settimeout(1)
    end = time.time() + int(os.environ.get("FAKE_SECS", "90"))
    print("fake noVNC on 6080", flush=True)
    while time.time() < end:
        try:
            conn, _ = listener.accept()
        except socket.timeout:
            continue
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


main()
