#!/usr/bin/env python3
"""Python counterpart of examples/smoke.rs; prints the same lines."""

import argparse

import lauterbach.trace32.rcl as rcl

parser = argparse.ArgumentParser()
parser.add_argument("--port", default="20000")
parser.add_argument("--address", default="E:0x20000000")
parser.add_argument("--length", type=int, default=32)
args = parser.parse_args()

debugger = rcl.connect(node="localhost", port=args.port, protocol="TCP", packlen=1024, timeout=5.0)
debugger.print("t32rcl smoke test")
print(f"system_up {str(debugger.fnc.system_up()).lower()}")
print(f"state_run {str(debugger.fnc.state_run()).lower()}")
data = debugger.memory.read(debugger.address.from_string(args.address), length=args.length)
print(f"read {args.address} {bytes(data).hex()}")
debugger.disconnect()
