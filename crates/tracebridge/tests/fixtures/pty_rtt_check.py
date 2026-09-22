#!/usr/bin/env python3
"""Run `tracebridge rtt` under a pseudo-terminal, press Ctrl-C, report terminal modes.

Usage: pty_rtt_check.py <tracebridge binary>. Used by tests/rtt_e2e.rs."""
import os, pty, sys, termios, time, socket, struct, threading, tempfile

# minimal fake RCL answering everything with success/zero memory (RTT stays uninitialized)
srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen(5); port = srv.getsockname()[1]
def serve(c):
    buf = b""
    while True:
        d = c.recv(65536)
        if not d: return
        buf += d
        while len(buf) >= 8:
            n = struct.unpack("<I", buf[:4])[0]; tot = (8+n+7)//8*8
            if len(buf) < tot: break
            data = buf[8:8+n]; buf = buf[tot:]
            cmd, opt, mid = data[1], data[2], data[3]
            pl = data[6:] if data[0] == 0 else data[4:]
            ans = bytes([0, mid])
            if cmd == 0x72 and opt == 5:
                e = pl[4:].split(b"\0")[0]
                v = {b"SOFTWARE.BUILD()": (8, b"190766."), b"SOFTWARE.BUILD.BASE()": (8, b"187884.")}.get(e, (0x40, b"OK"))
                ans += struct.pack("<II", v[0], len(v[1])) + v[1]
            elif cmd == 0x74 and opt == 0x35:
                ans += b"\0" * struct.unpack("<H", pl[:2])[0]
            f = struct.pack("<II", len(ans), 0x11) + ans
            c.sendall(f + b"\0" * ((8 - len(f) % 8) % 8))
threading.Thread(target=lambda: [threading.Thread(target=serve, args=(srv.accept()[0],), daemon=True).start() for _ in range(5)], daemon=True).start()

d = tempfile.mkdtemp()
open(os.path.join(d, "trace32.toml"), "w").write(f'[project]\nprogram = "demo"\n[trace32]\nrcl_port = {port}\n')
pid, master = pty.fork()
if pid == 0:
    os.chdir(d)
    os.execv(sys.argv[1], [sys.argv[1], "rtt", "--cb", "0x20000000"])
import select
out = b""
def drain(t):
    global out
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.05)
        if r:
            try: out += os.read(master, 4096)
            except OSError: return
drain(1.5)
a = termios.tcgetattr(master)
print("while running: ICANON", bool(a[3] & termios.ICANON), "ECHO", bool(a[3] & termios.ECHO), "ISIG", bool(a[3] & termios.ISIG), flush=True)
os.write(master, b"\x03")
status = None
for _ in range(100):
    drain(0.05)
    p, st = os.waitpid(pid, os.WNOHANG)
    if p:
        status = st; break
if status is None:
    print("child did not exit; killing", flush=True); os.kill(pid, 9); os.waitpid(pid, 0)
else:
    print("exit code", os.waitstatus_to_exitcode(status))
try:
    a = termios.tcgetattr(master)
    print("after ^C:      ICANON", bool(a[3] & termios.ICANON), "ECHO", bool(a[3] & termios.ECHO))
except Exception as e:
    print("tcgetattr after exit:", e)
print(repr(out.decode(errors="replace")[-300:]))
