#!/usr/bin/env python3
"""Drive a fixed RCL session with the Python library (lauterbach-trace32-rcl).

Used to record fixtures for crates/t32rcl/tests/replay.rs:

  1. cargo run -p t32rcl --example capture_proxy -- --out crates/t32rcl/tests/fixtures/<name>
     (listens on 20001, forwards to PowerView on 127.0.0.1:20000)
  2. python3 crates/t32rcl/tools/capture_session.py --port 20001 \
         --session crates/t32rcl/tests/fixtures/<name>/session.txt \
         [--address E:0x20000000] [--symbol '\\\\my_app\\Global\\_SEGGER_RTT']

The session file lists the operations in the order they ran, so the Rust test
can repeat them and compare every byte it sends with the recorded client bytes.
Memory writes put back exactly the bytes that were just read, so the target is
left unchanged.
"""

import argparse

import lauterbach.trace32.rcl as rcl


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--node", default="localhost")
    parser.add_argument("--port", type=int, default=20001)
    parser.add_argument("--session", required=True, help="session.txt to write")
    parser.add_argument("--address", default="", help="e.g. E:0x20000000")
    parser.add_argument("--length", type=int, default=16)
    parser.add_argument("--symbol", default="", help="e.g. \\\\my_app\\Global\\_SEGGER_RTT")
    args = parser.parse_args()

    lines = ["connect"]
    debugger = rcl.connect(node=args.node, port=str(args.port), protocol="TCP", packlen=1024, timeout=5.0)
    try:
        debugger.print("t32rcl capture")
        lines.append("print t32rcl capture")
        debugger.cmd("PRINT \"t32rcl capture\"")
        lines.append('cmd PRINT "t32rcl capture"')
        lines.append(f"system_up {debugger.fnc.system_up()}")
        lines.append(f"state_run {debugger.fnc.state_run()}")
        lines.append(f"fnc PRACTICE.SD() {debugger.fnc('PRACTICE.SD()')!r}")
        if args.address:
            address = debugger.address.from_string(args.address)
            data = bytes(debugger.memory.read(address, length=args.length))
            lines.append(f"read {args.address} {args.length} {data.hex()}")
            debugger.memory.write(address, data)
            lines.append(f"write {args.address} {data.hex()}")
            if len(data) >= 4:
                value = int.from_bytes(data[:4], "little")
                debugger.memory.write_uint32(address, value)
                lines.append(f"write_u32 {args.address} {value}")
        if args.symbol:
            value = debugger.symbol.query_by_name(name=args.symbol).address.value
            lines.append(f"symbol {args.symbol} {value}")
    finally:
        debugger.disconnect()

    with open(args.session, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
