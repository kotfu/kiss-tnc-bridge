#!/usr/bin/env python3
#
# Copyright (C) 2026 Jared Crapo
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""Watch kiss-tnc-bridge over a long run, for the soak test.

Every minute, a row of CSV: the daemon's memory (resident, peak, its own
data) and CPU from /proc, and the bridge's counters and port states from
'ktbctl status --json'. Meanwhile it's a KISS client of the bridge's
tcp-server port, counting the frames it gets, which should keep up with
the bridge's frames from the TNC. Nothing here needs root.

    scripts/soak.py run --bridge ktbr1 --kiss 127.0.0.1:8001 soak.csv
    scripts/soak.py summary soak.csv

Python 3 and its standard library only, so it runs on a stock Pi.
"""

import argparse
import csv
import json
import os
import socket
import subprocess
import sys
import threading
import time

FIELDS = [
    "time", "pid", "rss_kb", "hwm_kb", "anon_kb", "threads", "cpu_s",
    "frames_up", "frames_down", "dropped", "tnc_connects", "client_frames",
    "client_connects", "ports",
]


def daemon_pid():
    out = subprocess.run(["pgrep", "-x", "kiss-tnc-bridge"], capture_output=True, text=True)
    pids = out.stdout.split()
    return int(pids[0]) if pids else None


def proc(pid):
    """Memory in kB and CPU in seconds, from /proc."""
    status = {}
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            key, _, value = line.partition(":")
            status[key] = value.split()[0] if value.split() else ""
    with open(f"/proc/{pid}/stat") as f:
        # The command can hold spaces; the fields after it are fixed.
        fields = f.read().rsplit(")", 1)[1].split()
    ticks = os.sysconf("SC_CLK_TCK")
    return {
        "rss_kb": int(status.get("VmRSS", 0)),
        "hwm_kb": int(status.get("VmHWM", 0)),
        "anon_kb": int(status.get("RssAnon", 0)),
        "threads": int(status.get("Threads", 0)),
        "cpu_s": (int(fields[11]) + int(fields[12])) / ticks,
    }


def bridge(ktbctl, socket_path, bridge_id):
    """The bridge's counters, and each port as id=state/sessions."""
    out = subprocess.run(
        [ktbctl, "--socket", socket_path, "--json", "status"],
        capture_output=True, text=True, timeout=30,
    )
    if out.returncode != 0:
        return {}
    for b in json.loads(out.stdout)["bridges"]:
        if b["id"] == bridge_id:
            c = b.get("counters") or {}
            ports = " ".join(f"{p['id']}={p['state']}/{len(p['sessions'])}" for p in b["ports"])
            return {
                "frames_up": c.get("frames_up", ""),
                "frames_down": c.get("frames_down", ""),
                "dropped": c.get("dropped", ""),
                "tnc_connects": c.get("tnc_connects", ""),
                "ports": ports,
            }
    return {}


class Client(threading.Thread):
    """A KISS client that counts whole frames, and reconnects."""

    def __init__(self, address):
        super().__init__(daemon=True)
        host, port = address.rsplit(":", 1)
        self.address = (host, int(port))
        self.frames = 0
        self.connects = 0

    def run(self):
        while True:
            try:
                with socket.create_connection(self.address, timeout=10) as s:
                    s.settimeout(None)
                    self.connects += 1
                    in_frame = False
                    empty = True
                    while chunk := s.recv(4096):
                        for byte in chunk:
                            # A frame is the bytes between two FENDs. Frames
                            # come as FEND data FEND, so back-to-back FENDs
                            # hold nothing and don't count.
                            if byte == 0xC0:
                                if in_frame and not empty:
                                    self.frames += 1
                                in_frame = True
                                empty = True
                            else:
                                empty = False
            except OSError:
                pass
            time.sleep(5)


def run(args):
    client = None
    if args.kiss:
        client = Client(args.kiss)
        client.start()
    new = not os.path.exists(args.csv)
    with open(args.csv, "a", newline="") as f:
        out = csv.DictWriter(f, fieldnames=FIELDS)
        if new:
            out.writeheader()
        while True:
            row = {"time": time.strftime("%Y-%m-%dT%H:%M:%S")}
            pid = daemon_pid()
            if pid:
                row["pid"] = pid
                try:
                    row.update(proc(pid))
                except OSError:
                    pass
                try:
                    row.update(bridge(args.ktbctl, args.socket, args.bridge))
                except (subprocess.SubprocessError, ValueError, KeyError):
                    pass
            if client:
                row["client_frames"] = client.frames
                row["client_connects"] = client.connects
            out.writerow(row)
            f.flush()
            time.sleep(args.every)


def summary(args):
    with open(args.csv, newline="") as f:
        rows = [r for r in csv.DictReader(f) if r["rss_kb"]]
    if not rows:
        sys.exit("no samples yet")
    first, last = rows[0], rows[-1]
    hours = (time.mktime(time.strptime(last["time"], "%Y-%m-%dT%H:%M:%S"))
             - time.mktime(time.strptime(first["time"], "%Y-%m-%dT%H:%M:%S"))) / 3600
    num = lambda r, k: float(r[k]) if r.get(k) not in (None, "") else 0.0
    print(f"{len(rows)} samples over {hours:.1f} h, {first['time']} to {last['time']}")
    pids = sorted({r["pid"] for r in rows})
    print(f"daemon pid {', '.join(pids)}" + ("  (it restarted)" if len(pids) > 1 else ""))
    for k, what in [("rss_kb", "resident"), ("anon_kb", "its own data"), ("hwm_kb", "peak resident")]:
        print(f"{what:>15}: {num(first, k):.0f} kB at the start, {num(last, k):.0f} kB at the end, "
              f"{max(num(r, k) for r in rows):.0f} kB at most")
    cpu = num(last, "cpu_s") - num(first, "cpu_s")
    if hours > 0:
        print(f"{'CPU':>15}: {cpu:.1f} s, {100 * cpu / (hours * 3600):.2f}% of a core")
    for k in ["frames_down", "frames_up", "dropped", "tnc_connects", "client_frames", "client_connects"]:
        print(f"{k:>15}: {num(first, k):.0f} to {num(last, k):.0f}")
    down = num(last, "frames_down") - num(first, "frames_down")
    got = num(last, "client_frames") - num(first, "client_frames")
    if last.get("client_frames"):
        print(f"the TCP client got {got:.0f} of the {down:.0f} frames from the TNC"
              + ("" if got >= down else "  (fewer: was it disconnected?)"))
    changes = [(r["time"], r["ports"]) for i, r in enumerate(rows) if i == 0 or r["ports"] != rows[i - 1]["ports"]]
    print("ports:")
    for t, p in changes:
        print(f"  {t}  {p}")


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="sample until stopped")
    r.add_argument("csv")
    r.add_argument("--bridge", required=True, help="the bridge to watch: ktbr1")
    r.add_argument("--kiss", help="its tcp-server, to count frames as a client: 127.0.0.1:8001")
    r.add_argument("--socket", default="./run/control.sock")
    r.add_argument("--ktbctl", default="./ktbctl")
    r.add_argument("--every", type=float, default=60, help="seconds between samples [60]")
    s = sub.add_parser("summary", help="what a run showed")
    s.add_argument("csv")
    args = p.parse_args()
    run(args) if args.cmd == "run" else summary(args)


if __name__ == "__main__":
    main()
