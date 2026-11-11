#!/usr/bin/env python3
"""The spike's console, for running it unattended: prints every line from the
ATOM with the host's time, and sends it each line written to a FIFO.

    python3 tools/console.py [-p PORT] [--reset] [--fifo PATH] > spike.log
    echo beacon > PATH

With --reset the ATOM restarts first, so the log has the start-up heap lines.
"""

import argparse
import datetime
import os
import stat
import sys
import threading
import time

import serial
import serial.tools.list_ports

FTDI = (0x0403, 0x6001)


def find_port():
    ports = [p.device for p in serial.tools.list_ports.comports() if (p.vid, p.pid) == FTDI]
    if len(ports) != 1:
        sys.exit("need exactly one FTDI port, found: " + (", ".join(ports) or "none"))
    return ports[0]


def open_port(path, reset):
    # As firmware/tools/ktbdev.py: RTS alone resets the ATOM, so drop RTS
    # before DTR.
    s = serial.Serial()
    s.port = path
    s.baudrate = 115200
    s.timeout = 0.1
    s.dtr = True
    s.rts = False
    s.open()
    s.dtr = False
    if reset:
        s.rts = True
        time.sleep(0.1)
        s.rts = False
    return s


def stamp():
    return datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]


def commands(s, fifo):
    while True:
        with open(fifo) as f:
            for line in f:
                line = line.strip()
                if line:
                    print(f"{stamp()} >>> {line}", flush=True)
                    s.write((line + "\n").encode())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-p", "--port")
    ap.add_argument("--reset", action="store_true")
    ap.add_argument("--fifo", default="/tmp/ktb-spike.fifo")
    a = ap.parse_args()
    if not (os.path.exists(a.fifo) and stat.S_ISFIFO(os.stat(a.fifo).st_mode)):
        os.mkfifo(a.fifo)
    s = open_port(a.port or find_port(), a.reset)
    threading.Thread(target=commands, args=(s, a.fifo), daemon=True).start()
    buf = b""
    while True:
        buf += s.read(512)
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            print(f"{stamp()} {line.decode(errors='replace').rstrip()}", flush=True)


if __name__ == "__main__":
    main()
