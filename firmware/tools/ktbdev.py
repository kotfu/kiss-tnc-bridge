#!/usr/bin/env python3
"""Development-only commands for the firmware over USB serial: the ones
ktbctl doesn't have.

Everything else goes through ktbctl, so the path people use is the one we
use too:

    ktbctl device info
    ktbctl --device PORT status
    ktbctl device settings set led_normal=dark
    ktbctl --device PORT monitor

What's left here: the debug.* commands (development build), the LED demo,
and the raw log. The port is opened with DTR and RTS deasserted: they drive
the ESP32's reset and boot pins (docs/control-api.md, USB serial). Needs
pyserial, which ESP-IDF's Python environment has:

    . ~/src/esp/esp-idf/export.sh
    firmware/tools/ktbdev.py debug radio=connected phones=1
    firmware/tools/ktbdev.py frames to_radio 5
    firmware/tools/ktbdev.py demo
    firmware/tools/ktbdev.py log
"""

import argparse
import json
import sys
import time

import serial
import serial.tools.list_ports

# The boards' USB serial chips: the ATOM Lite's CH552 shows FTDI's ids, the
# Core2's is a CH9102.
CHIPS = {(0x0403, 0x6001), (0x1A86, 0x55D4)}


def find_port():
    ports = [p for p in serial.tools.list_ports.comports() if (p.vid, p.pid) in CHIPS]
    if not ports:
        sys.exit("no ATOM Lite or Core2 found; give the port with -p")
    if len(ports) > 1:
        sys.exit("more than one device: " + ", ".join(p.device for p in ports) + "; pick one with -p")
    return ports[0].device


def open_port(path):
    # The ATOM's reset circuit pulls EN low only while RTS is asserted and
    # DTR isn't. The OS may assert both on open, and pyserial sets DTR
    # before RTS, so deasserting both that way passes through "RTS alone"
    # and reboots the device. Drop RTS first (DTR alone only touches the
    # boot pin, which matters only at reset), then DTR.
    s = serial.Serial()
    s.port = path
    s.baudrate = 115200
    s.timeout = 0.1
    s.dtr = True
    s.rts = False
    s.open()
    s.dtr = False
    return s


class Device:
    def __init__(self, port, verbose):
        self.s = open_port(port)
        self.verbose = verbose
        # Unique across runs: the device may still answer an earlier run's
        # requests.
        self.next_id = int(time.time() * 1000) % 1_000_000_000 * 1000
        self.buf = b""

    def lines(self, until):
        while time.monotonic() < until:
            chunk = self.s.read(256)
            self.buf += chunk
            while b"\n" in self.buf:
                line, self.buf = self.buf.split(b"\n", 1)
                yield line.rstrip(b"\r").decode("utf-8", "replace")

    def request(self, cmd, timeout=3.0, **params):
        """Sends a request and returns its response."""
        rid = self.next_id
        self.next_id += 1
        self.s.write((json.dumps({"v": 1, "id": rid, "cmd": cmd, **params}) + "\n").encode())
        for line in self.lines(time.monotonic() + timeout):
            if not line.startswith("{"):
                if self.verbose:
                    print("  log:", line, file=sys.stderr)
                continue
            try:
                reply = json.loads(line)
            except json.JSONDecodeError:
                continue
            if reply.get("id") == rid:
                return reply
        sys.exit(f"no answer to {cmd} within {timeout} s")


def key_values(pairs):
    out = {}
    for p in pairs:
        if "=" not in p:
            sys.exit(f"expected key=value, got {p!r}")
        k, v = p.split("=", 1)
        try:
            out[k] = json.loads(v)
        except json.JSONDecodeError:
            out[k] = v
    return out


def show(reply):
    print(json.dumps(reply.get("result") if reply.get("ok") else reply, indent=2))
    return 0 if reply.get("ok") else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("-p", "--port", help="serial port (default: the one ATOM Lite or Core2)")
    ap.add_argument("-v", "--verbose", action="store_true", help="print the firmware's log too")
    sub = ap.add_subparsers(dest="what", required=True)
    p = sub.add_parser("debug", help="debug.set radio=none|down|connected|real phones=N error=true updating=true modem_sleep=false spin_cores=0|1|2 (the last two are kept)")
    p.add_argument("pairs", nargs="+")
    p = sub.add_parser("frames", help="debug.frame: COUNT frames to_phone or to_radio")
    p.add_argument("direction", choices=["to_phone", "to_radio"])
    p.add_argument("count", nargs="?", type=int, default=1)
    p.add_argument("--every", type=float, default=0.5, help="seconds between frames")
    p = sub.add_parser("demo", help="show every LED state and event in turn (development build)")
    p.add_argument("--seconds", type=float, default=5, help="how long to show each")
    p.add_argument("--breathe-seconds", type=float, default=20,
                   help="how long to show each breathing color (a breath is 8 s)")
    sub.add_parser("log", help="print everything the device sends, its log included")
    args = ap.parse_args()

    port = args.port or find_port()
    if args.what == "log":
        s = open_port(port)
        try:
            while True:
                data = s.read(256)
                if data:
                    sys.stdout.write(data.decode("utf-8", "replace"))
                    sys.stdout.flush()
        except KeyboardInterrupt:
            return 0

    d = Device(port, args.verbose)
    if args.what == "debug":
        return show(d.request("debug.set", **key_values(args.pairs)))
    if args.what == "frames":
        for i in range(args.count):
            reply = d.request("debug.frame", direction=args.direction)
            if not reply.get("ok"):
                return show(reply)
            if i + 1 < args.count:
                time.sleep(args.every)
        return 0
    if args.what == "demo":
        return demo(d, args.seconds, args.breathe_seconds)
    return 2


def demo(d, seconds, breathe_seconds):
    """Every state and event in turn, for judging the LED by eye."""

    def step(label, cmd, wait=None, **params):
        print(f"{label:<44}", end="", flush=True)
        reply = d.request(cmd, **params)
        if not reply.get("ok"):
            print(reply["error"]["message"])
            return False
        time.sleep(seconds if wait is None else wait)
        print("done")
        return True

    original = d.request("device.settings.get")["result"]["settings"]["led_normal"]
    try:
        # Bright, so the normal states and the frames show whatever mode the
        # device was left in (dark mode would hide them).
        d.request("device.settings.set", settings={"led_normal": "bright"})
        d.request("debug.set", radio="none", phones=0, error=False, updating=False)
        d.request("auth.mode", seconds=0)
        step("state: no radio (yellow/blue, slow)", "debug.set", radio="none")
        step("state: radio not connected (yellow, slow)", "debug.set", radio="down")
        step("state: connected, no phones (cyan)", "debug.set", radio="connected", phones=0)
        step("state: connected, phones (green)", "debug.set", phones=1)
        print(f"{'  frames to the phone (dips)':<44}", end="", flush=True)
        for _ in range(8):
            d.request("debug.frame", direction="to_phone")
            time.sleep(1.0)
        print("done")
        print(f"{'  frames to the radio (white flicks)':<44}", end="", flush=True)
        for _ in range(8):
            d.request("debug.frame", direction="to_radio")
            time.sleep(1.0)
        print("done")
        d.request("device.settings.set", settings={"led_normal": "dim"})
        step("  dim, green", "debug.set", phones=1)
        step("  dim, cyan", "debug.set", phones=0)
        d.request("device.settings.set", settings={"led_normal": "breathe"})
        step("  breathe, green", "debug.set", wait=breathe_seconds, phones=1)
        step("  breathe, cyan", "debug.set", wait=breathe_seconds, phones=0)
        d.request("device.settings.set", settings={"led_normal": "bright"})
        step("state: authorization mode (magenta, slow)", "auth.mode", seconds=600)
        d.request("auth.mode", seconds=0)
        step("state: updating (white, fast)", "debug.set", updating=True)
        d.request("debug.set", updating=False)
        step("state: error (red, three blinks)", "debug.set", error=True)
        d.request("debug.set", error=False)
        step("event: identify", "device.identify")
    finally:
        d.request("device.settings.set", settings={"led_normal": original})
        d.request("debug.set", radio="real", phones=-1)
    print("Pairing: double press (blue), then a single press cancels it (yellow flashes).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
