#!/usr/bin/env python3
"""Minimal NETTCP RCL server that answers the requests capture_session.py makes.

It records both directions, so running the Python library against it produces a
fixture of the exact bytes the Python library sends (crates/t32rcl/tests/
fixtures/python-fake). The answers are synthetic; real PowerView answers come
from captures made with the capture_proxy example.

  python3 fake_t32.py --port 20002 --out <fixture dir> &
  python3 capture_session.py --port 20002 --session <fixture dir>/session.txt \
      --address E:0x20000000 --symbol '\\\\demo\\Global\\_SEGGER_RTT'
"""

import argparse
import os
import socket
import struct

FUNCTIONS = {
    "SOFTWARE.BUILD()": (0x0008, "190766."),
    "SOFTWARE.BUILD.BASE()": (0x0008, "187884."),
    "VERSION.PYRCL(1.1.5)": (0x0040, "OK"),
    "SYStem.Up()": (0x0001, "TRUE()"),
    "STATE.RUN()": (0x0001, "FALSE()"),
    "PRACTICE.SD()": (0x0008, "0."),
}


def frame(payload: bytes) -> bytes:
    data = struct.pack("<II", len(payload), 0x11) + payload
    return data + b"\0" * ((8 - len(data) % 8) % 8)


def answer(data: bytes) -> bytes:
    if data[0] == 0:
        rapi_cmd, opt_arg, message_id = data[1], data[2], data[3]
        payload = data[6:]
    else:
        rapi_cmd, opt_arg, message_id = data[1], data[2], data[3]
        payload = data[4:]
    ok = bytes([0, message_id])
    if rapi_cmd == 0x71:
        return ok
    if rapi_cmd == 0x72 and opt_arg == 0x04:
        return ok
    if rapi_cmd == 0x72 and opt_arg == 0x05:
        expression = payload[4:].split(b"\0", 1)[0].decode()
        result_type, value = FUNCTIONS[expression]
        return ok + struct.pack("<II", result_type, len(value)) + value.encode()
    if rapi_cmd == 0x74 and opt_arg == 0x35:
        length = struct.unpack("<H", payload[:2])[0]
        return ok + bytes((index * 3 + 1) & 0xFF for index in range(length))
    if rapi_cmd == 0x74 and opt_arg == 0x36:
        return ok
    if rapi_cmd == 0x74 and opt_arg == 0x68:
        body = b"NM\x0c\x00_SEGGER_RTT\x00AD" + struct.pack("<HQ", 3, 0x20000400)
        body += b"AC\x02\x00D\x00XXSZ" + struct.pack("<Q", 168) + b"XX"
        return ok + body
    raise ValueError(f"unexpected request {data.hex()}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=20002)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)

    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", args.port))
    server.listen(1)
    connection, _ = server.accept()
    client_log = open(os.path.join(args.out, "client.bin"), "wb")
    server_log = open(os.path.join(args.out, "server.bin"), "wb")
    buffer = b""
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            break
        client_log.write(chunk)
        buffer += chunk
        while len(buffer) >= 8:
            length = struct.unpack("<I", buffer[:4])[0]
            total = 8 + length + (8 - (8 + length) % 8) % 8
            if len(buffer) < total:
                break
            reply = frame(answer(buffer[8 : 8 + length]))
            buffer = buffer[total:]
            server_log.write(reply)
            connection.sendall(reply)
    client_log.close()
    server_log.close()


if __name__ == "__main__":
    main()
