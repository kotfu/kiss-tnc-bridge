#!/usr/bin/env python3
"""Talks to the firmware's control API over USB serial, for development.

The port is opened with DTR and RTS deasserted: on the ATOM Lite they drive
the ESP32's reset and boot pins (docs/control-api.md, USB serial). Lines
that don't start with '{' are the firmware's log; -v prints them.

Needs pyserial, which ESP-IDF's Python environment has:

    . ~/src/esp/esp-idf/export.sh
    firmware/tools/ktbdev.py info
    firmware/tools/ktbdev.py set led_normal=dark led_wake_seconds=10
    firmware/tools/ktbdev.py debug radio=connected phones=1
    firmware/tools/ktbdev.py monitor

This is a stand-in until ktbctl manages devices (Phase D).
"""

import argparse
import json
import sys
import time

import serial
import serial.tools.list_ports

FTDI = (0x0403, 0x6001)  # the ATOM Lite's USB serial chip


def find_port():
    ports = [p for p in serial.tools.list_ports.comports() if (p.vid, p.pid) == FTDI]
    if not ports:
        sys.exit("no ATOM Lite found; give the port with -p")
    if len(ports) > 1:
        sys.exit("more than one FTDI port: " + ", ".join(p.device for p in ports) + "; pick one with -p")
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
        self.next_id = 1
        self.buf = b""

    def lines(self, until):
        while time.monotonic() < until:
            chunk = self.s.read(256)
            self.buf += chunk
            while b"\n" in self.buf:
                line, self.buf = self.buf.split(b"\n", 1)
                yield line.rstrip(b"\r").decode("utf-8", "replace")

    def send(self, cmd, **params):
        rid = self.next_id
        self.next_id += 1
        msg = {"v": 1, "id": rid, "cmd": cmd, **params}
        self.s.write((json.dumps(msg) + "\n").encode())
        return rid

    def request(self, cmd, timeout=3.0, on_event=None, **params):
        """Sends a request and returns its response. Events go to on_event
        (which may send requests of its own) or are printed."""
        rid = self.send(cmd, **params)
        return self.wait(rid, cmd, timeout, on_event)

    def wait(self, rid, cmd, timeout, on_event=None):
        for line in self.lines(time.monotonic() + timeout):
            if not line.startswith("{"):
                if self.verbose:
                    print("  log:", line, file=sys.stderr)
                continue
            try:
                reply = json.loads(line)
            except json.JSONDecodeError:
                print("  bad JSON:", line, file=sys.stderr)
                continue
            if reply.get("id") == rid:
                return reply
            if "event" in reply:
                if on_event:
                    on_event(reply)
                else:
                    print_event(reply)
                continue
            if self.verbose:
                print("  other:", line, file=sys.stderr)
        sys.exit(f"no answer to {cmd} within {timeout} s")


def print_event(e):
    kind = e.get("event")
    if kind == "frame":
        d = e.get("decoded", {})
        print(f"frame {e.get('dir')}: {d.get('summary') or e.get('kiss')}", flush=True)
    elif kind == "state":
        print(f"state: {e.get('object')} {e.get('state')}", flush=True)
    elif kind == "found":
        dev = e["device"]
        extra = f" ({dev['known_tnc']})" if "known_tnc" in dev else ""
        print(f"found {dev['address']} {dev.get('name', '?')}{extra} rssi {dev.get('rssi', '?')}", flush=True)
    elif kind == "pairing":
        print("pairing:", e.get("step"), e.get("name") or e.get("passkey") or "", flush=True)
    else:
        print("event:", json.dumps(e), flush=True)


def parse_value(text):
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def key_values(pairs):
    out = {}
    for p in pairs:
        if "=" not in p:
            sys.exit(f"expected key=value, got {p!r}")
        k, v = p.split("=", 1)
        out[k] = parse_value(v)
    return out


def show(reply):
    print(json.dumps(reply.get("result") if reply.get("ok") else reply, indent=2))
    return 0 if reply.get("ok") else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("-p", "--port", help="serial port (default: the one FTDI port)")
    ap.add_argument("-v", "--verbose", action="store_true", help="print the firmware's log too")
    sub = ap.add_subparsers(dest="what", required=True)
    sub.add_parser("info", help="device.info")
    sub.add_parser("settings", help="device.settings.get")
    p = sub.add_parser("set", help="device.settings.set key=value ...")
    p.add_argument("pairs", nargs="+")
    sub.add_parser("auth", help="auth.info")
    sub.add_parser("code", help="auth.code")
    sub.add_parser("rotate-code", help="auth.rotate_code")
    p = sub.add_parser("window", help="auth.window (seconds; 0 closes it)")
    p.add_argument("seconds", nargs="?", type=int)
    sub.add_parser("clients", help="auth.clients")
    sub.add_parser("identify", help="device.identify")
    sub.add_parser("reboot", help="device.reboot")
    sub.add_parser("reset", help="device.reset: clears settings, pairing, clients")
    p = sub.add_parser("raw", help="send a command with JSON parameters")
    p.add_argument("cmd")
    p.add_argument("params", nargs="?", default="{}")
    p = sub.add_parser("debug", help="debug.set radio=none|down|connected|real phones=N error=true updating=true")
    p.add_argument("pairs", nargs="+")
    p = sub.add_parser("frames", help="debug.frame: COUNT frames to_phone or to_radio")
    p.add_argument("direction", choices=["to_phone", "to_radio"])
    p.add_argument("count", nargs="?", type=int, default=1)
    p.add_argument("--every", type=float, default=0.5, help="seconds between frames")
    p = sub.add_parser("demo", help="show every LED state and event in turn (debug build)")
    p.add_argument("--seconds", type=float, default=5, help="how long to show each")
    p.add_argument("--breathe-seconds", type=float, default=20,
                   help="how long to show each breathing color (a breath is 8 s)")
    sub.add_parser("monitor", help="print everything the device sends")
    sub.add_parser("status", help="status")
    sub.add_parser("models", help="tnc.models")
    p = sub.add_parser("scan", help="bluetooth.scan for SECONDS (default 10)")
    p.add_argument("seconds", nargs="?", type=int, default=10)
    p = sub.add_parser("pair", help="bluetooth.pair ADDRESS (default: the radio's), confirming the number")
    p.add_argument("address", nargs="?")
    p.add_argument("--yes", action="store_true", help="accept the number without asking")
    p = sub.add_parser("unpair", help="bluetooth.unpair ADDRESS (default: the radio)")
    p.add_argument("address", nargs="?")
    p = sub.add_parser("tnc", help="tnc.update model=th-d75 address=40:79:12:E4:95:9D")
    p.add_argument("pairs", nargs="+")
    p.add_argument("--non-disruptive", action="store_true")
    p = sub.add_parser("port", help="port.connect, port.disconnect or port.release for the radio")
    p.add_argument("action", choices=["connect", "disconnect", "release"])
    p = sub.add_parser("watch", help="subscribe and print events (default topics: state frames)")
    p.add_argument("topics", nargs="*", default=["state", "frames"])
    p = sub.add_parser("unsubscribe", help="unsubscribe from TOPICS (default: all)")
    p.add_argument("topics", nargs="*")
    args = ap.parse_args()

    port = args.port or find_port()
    if args.what == "monitor":
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
    w = args.what
    if w == "info":
        return show(d.request("device.info"))
    if w == "settings":
        return show(d.request("device.settings.get"))
    if w == "set":
        return show(d.request("device.settings.set", settings=key_values(args.pairs)))
    if w == "auth":
        return show(d.request("auth.info"))
    if w == "code":
        return show(d.request("auth.code"))
    if w == "rotate-code":
        return show(d.request("auth.rotate_code"))
    if w == "window":
        params = {} if args.seconds is None else {"seconds": args.seconds}
        return show(d.request("auth.window", **params))
    if w == "clients":
        return show(d.request("auth.clients"))
    if w == "identify":
        return show(d.request("device.identify"))
    if w == "reboot":
        return show(d.request("device.reboot"))
    if w == "reset":
        return show(d.request("device.reset"))
    if w == "raw":
        return show(d.request(args.cmd, **json.loads(args.params)))
    if w == "debug":
        return show(d.request("debug.set", **key_values(args.pairs)))
    if w == "frames":
        for i in range(args.count):
            reply = d.request("debug.frame", direction=args.direction)
            if not reply.get("ok"):
                return show(reply)
            if i + 1 < args.count:
                time.sleep(args.every)
        return 0
    if w == "status":
        return show(d.request("status"))
    if w == "models":
        return show(d.request("tnc.models"))
    if w == "scan":
        return show(d.request("bluetooth.scan", timeout=args.seconds + 10, seconds=args.seconds))
    if w == "pair":
        return pair(d, args.address, args.yes)
    if w == "unpair":
        params = {"address": args.address} if args.address else {}
        return show(d.request("bluetooth.unpair", timeout=10, **params))
    if w == "tnc":
        kv = key_values(args.pairs)
        definition = {}
        if "model" in kv:
            definition["model"] = kv.pop("model")
        if "address" in kv:
            definition["spp"] = {"address": kv.pop("address")}
        if kv:
            sys.exit("tnc takes model= and address=")
        return show(d.request("tnc.update", timeout=10, tnc="radio", definition=definition,
                              non_disruptive=args.non_disruptive))
    if w == "port":
        return show(d.request("port." + args.action, timeout=10, bridge="ktbr0", port="radio"))
    if w == "watch":
        reply = d.request("subscribe", topics=args.topics)
        if not reply.get("ok"):
            return show(reply)
        print("watching", ", ".join(reply["result"]["topics"]), "(Ctrl-C to stop)", flush=True)
        try:
            while True:
                for line in d.lines(time.monotonic() + 1):
                    if line.startswith("{"):
                        try:
                            print_event(json.loads(line))
                        except json.JSONDecodeError:
                            pass
                    elif args.verbose:
                        print("  log:", line, file=sys.stderr)
        except KeyboardInterrupt:
            pass
        finally:
            # USB serial can't tell the device the connection closed.
            d.send("unsubscribe", topics=args.topics)
            time.sleep(0.2)
        return 0
    if w == "unsubscribe":
        params = {"topics": args.topics} if args.topics else {}
        return show(d.request("unsubscribe", **params))
    if w == "demo":
        return demo(d, args.seconds, args.breathe_seconds)
    return 2


def pair(d, address, yes):
    """bluetooth.pair, answering the radio's number."""

    rid_holder = {}

    def on_event(e):
        print_event(e)
        if e.get("event") == "pairing" and e.get("step") == "confirm":
            accept = yes or input(f"Does the radio show {e['passkey']}? [y/n] ").strip().lower() == "y"
            if accept:
                print("Press OK on the radio now.", flush=True)
            d.send("bluetooth.pair_reply", request=rid_holder["id"], accept=accept)

    params = {"address": address} if address else {}
    rid_holder["id"] = d.send("bluetooth.pair", **params)
    return show(d.wait(rid_holder["id"], "bluetooth.pair", 120, on_event))


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
        # Solid, so the normal states and the frames show whatever mode the
        # device was left in (dark mode would hide them).
        d.request("device.settings.set", settings={"led_normal": "solid"})
        d.request("debug.set", radio="none", phones=0, error=False, updating=False)
        d.request("auth.window", seconds=0)
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
        d.request("device.settings.set", settings={"led_normal": "solid"})
        step("state: window open (magenta, slow)", "auth.window", seconds=600)
        d.request("auth.window", seconds=0)
        step("state: updating (white, fast)", "debug.set", updating=True)
        d.request("debug.set", updating=False)
        step("state: error (red, fast)", "debug.set", error=True)
        d.request("debug.set", error=False)
        step("event: identify", "device.identify")
    finally:
        d.request("device.settings.set", settings={"led_normal": original})
        d.request("debug.set", radio="real", phones=0)
    print("Pairing: double press (blue), then a single press cancels it (yellow flashes).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
