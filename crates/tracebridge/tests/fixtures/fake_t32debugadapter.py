#!/usr/bin/env python3
"""Stand-in for t32debugadapter in end-to-end tests.

Listens on --port, accepts one DAP connection and answers every request with
success. `scopes` returns a Locals scope (reference 42) and a Registers scope
(reference 43); `variables` returns one variable, so the test can tell whether
the proxy answered it instead.
"""

import argparse
import json
import socket


def send(connection, message):
    body = json.dumps(message).encode()
    connection.sendall(b"Content-Length: %d\r\n\r\n" % len(body) + body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log_to")
    parser.add_argument("--log_level")
    args = parser.parse_args()
    print(f"fake t32debugadapter on {args.port}", flush=True)

    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", args.port))
    server.listen(1)
    connection, _ = server.accept()
    buffer = b""
    seq = 1
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            return
        buffer += chunk
        while b"\r\n\r\n" in buffer:
            header, rest = buffer.split(b"\r\n\r\n", 1)
            length = int(header.split(b":", 1)[1])
            if len(rest) < length:
                break
            request = json.loads(rest[:length])
            buffer = rest[length:]
            body = {}
            if request["command"] == "scopes":
                body = {"scopes": [
                    {"name": "Locals", "presentationHint": "locals", "variablesReference": 42},
                    {"name": "Registers", "variablesReference": 43},
                ]}
            elif request["command"] == "variables":
                body = {"variables": [{"name": "r0", "value": "1", "variablesReference": 0}]}
            send(connection, {
                "seq": seq, "type": "response", "request_seq": request["seq"],
                "success": True, "command": request["command"], "body": body,
            })
            seq += 1


if __name__ == "__main__":
    main()
