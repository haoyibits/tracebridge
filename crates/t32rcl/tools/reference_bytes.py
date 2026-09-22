import struct
from lauterbach.trace32.rcl._rc._library import Library
from lauterbach.trace32.rcl._rc._address import Address
from lauterbach.trace32.rcl._rc._symbol import Symbol
from lauterbach.trace32.rcl._rc.hlinknet import Link, CommunicationTcp
from lauterbach.trace32.rcl._rc.common import align_eight

class FakeLink:
    packlen = 1024
    def __init__(self): self.mid = 0; self.sent = []
    def increment_message_id(self, n=1): self.mid += n; return self.mid % 255
    def get_message_id(self): return self.mid % 255
    def getHeadersize(self): return 10
    def transmit(self, data):
        size = len(data)
        frame = struct.pack("II{}s{}x".format(size, align_eight(size + 8)), size, 0x10, data)
        self.sent.append(frame)
    def receive(self):
        # minimal OK response: [err=0][msgid] + 30 zero bytes
        return bytes([0, self.get_message_id()]) + b"\0" * 30

lib = object.__new__(Library)
link = FakeLink()
lib._link = link
lib._maxpacketsize = link.packlen
def show(name, fn):
    link.sent.clear(); fn(); 
    for f in link.sent: print(f"{name:32s} {f.hex(' ')}")
show("attach(1)", lambda: lib.t32_attach(1))
show("cmd('Go')", lambda: lib.t32_executecommand(b"Go", 4096))
show("fnc('SYStem.Up()')", lambda: lib.t32_executefunction("SYStem.Up()"))
a = Address(None, access="E", value=0x20000000)
show("read E:0x20000000 len16", lambda: lib.t32_readmemoryobj(a, 16))
show("write E:0x20000000 b'AB'", lambda: lib.t32_writememoryobj(b"AB", a))
show("write_uint32 (width=4) 7", lambda: lib.t32_writememoryobj(struct.pack("<I",7), a, 4, width=4))
s = Symbol(None, name="\\\\app\\Global\\_SEGGER_RTT")
show("symbol query", lambda: lib.t32_querysymbolobj(s))
link.mid = 253
show("id 254 then wrap", lambda: (lib.t32_attach(1), lib.t32_attach(1), lib.t32_attach(1)))
