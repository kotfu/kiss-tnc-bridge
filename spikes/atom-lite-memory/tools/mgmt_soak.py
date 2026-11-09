"""Second BLE client for the ATOM Lite memory spike.

Connects to the spike, subscribes to the stand-in management service (and,
with --kiss, to the KISS service too), and sends length-prefixed messages of
growing size in MTU-sized write-without-response chunks. Each reply carries
the device's heap numbers, which are printed one line per message.

Needs bleak (pip install bleak). Runs on macOS, Linux or Windows.
"""

import argparse
import asyncio
import json
import struct
import time

from bleak import BleakClient, BleakScanner

MGMT_TX = "4b544201-9b3e-4a51-8d36-5c3a1f7e0002"
MGMT_RX = "4b544201-9b3e-4a51-8d36-5c3a1f7e0003"
KISS_RX = "00000003-ba2a-46c9-ae49-01b0961f68bb"


class Reassembler:
    """Splits a notification stream into length-prefixed messages."""

    def __init__(self):
        self.buf = bytearray()

    def feed(self, data):
        self.buf += data
        out = []
        while len(self.buf) >= 4:
            (n,) = struct.unpack(">I", self.buf[:4])
            if len(self.buf) < 4 + n:
                break
            out.append(bytes(self.buf[4 : 4 + n]))
            del self.buf[: 4 + n]
        return out


def stamp():
    return time.strftime("%H:%M:%S")


async def run(args):
    print(f"{stamp()} scanning for '{args.name}'", flush=True)
    dev = await BleakScanner.find_device_by_name(args.name, timeout=20)
    if dev is None:
        raise SystemExit(f"'{args.name}' not found")

    async with BleakClient(dev) as client:
        chunk = client.mtu_size - 3
        print(f"{stamp()} connected, MTU {client.mtu_size}, chunks of {chunk}", flush=True)

        replies = asyncio.Queue()
        reasm = Reassembler()
        kiss_bytes = 0

        def on_mgmt(_, data):
            for body in reasm.feed(data):
                replies.put_nowait(body)

        def on_kiss(_, data):
            nonlocal kiss_bytes
            kiss_bytes += len(data)

        await client.start_notify(MGMT_RX, on_mgmt)
        if args.kiss:
            await client.start_notify(KISS_RX, on_kiss)

        sizes = [int(s) for s in args.sizes.split(",")]
        deadline = time.monotonic() + args.duration if args.duration else None
        sent = 0
        while deadline is None or time.monotonic() < deadline:
            n = sizes[sent % len(sizes)]
            body = (b'{"cmd":"status","pad":"' + b"x" * n)[:n]
            msg = struct.pack(">I", len(body)) + body
            start = time.monotonic()
            for i in range(0, len(msg), chunk):
                await client.write_gatt_char(MGMT_TX, msg[i : i + chunk], response=False)
            try:
                reply = json.loads(await asyncio.wait_for(replies.get(), 10))
            except asyncio.TimeoutError:
                print(f"{stamp()} sent {n} B: no reply in 10 s", flush=True)
            else:
                rtt = (time.monotonic() - start) * 1000
                ok = "ok" if reply.get("echo_len") == n else f"MISMATCH echo_len={reply.get('echo_len')}"
                print(
                    f"{stamp()} sent {n:5d} B {ok} rtt={rtt:6.0f} ms heap free={reply['heap_free']}"
                    f" min={reply['heap_min']} largest={reply['heap_largest']} kiss_rx={kiss_bytes} B",
                    flush=True,
                )
            sent += 1
            await asyncio.sleep(args.interval)


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--name", default="KTB ATOM spike")
    p.add_argument("--sizes", default="16,500,2000,4096", help="message sizes to cycle through, bytes")
    p.add_argument("--interval", type=float, default=5, help="seconds between messages")
    p.add_argument("--duration", type=float, default=0, help="seconds to run; 0 runs until stopped")
    p.add_argument("--kiss", action="store_true", help="also subscribe to KISS notifications")
    asyncio.run(run(p.parse_args()))


if __name__ == "__main__":
    main()
