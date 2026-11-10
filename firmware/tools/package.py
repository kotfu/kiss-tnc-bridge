#!/usr/bin/env python3
"""Packages a firmware build for a release: the four images under their
release names, and the manifest the web installer reads
(docs/web-installer.md, What a release publishes).

    tools/package.py BUILD_DIR BOARD VERSION OUT_DIR

Reads the offsets and flash settings from BUILD_DIR/flasher_args.json, so
they're always the ones the build used. Standard library only.
"""

import argparse
import datetime
import hashlib
import json
import pathlib
import shutil
import sys

# flasher_args.json's names for the parts, and ours, in flash order.
PARTS = [
    ("bootloader", "bootloader"),
    ("partition-table", "partition-table"),
    ("otadata", "ota-data-initial"),
    ("app", "app"),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("build_dir", type=pathlib.Path)
    ap.add_argument("board")
    ap.add_argument("version")
    ap.add_argument("out_dir", type=pathlib.Path)
    args = ap.parse_args()

    flasher = json.loads((args.build_dir / "flasher_args.json").read_text())
    settings = flasher["flash_settings"]
    args.out_dir.mkdir(parents=True, exist_ok=True)
    prefix = f"ktb-{args.board}-{args.version}"

    parts = []
    for key, name in PARTS:
        entry = flasher.get(key)
        if not entry:
            sys.exit(f"{key} missing from flasher_args.json")
        src = args.build_dir / entry["file"]
        data = src.read_bytes()
        dest = f"{prefix}-{name}.bin"
        shutil.copyfile(src, args.out_dir / dest)
        parts.append({
            "path": dest,
            "offset": int(entry["offset"], 16),
            "sha256": hashlib.sha256(data).hexdigest(),
            "size": len(data),
        })

    manifest = {
        "name": "kiss-tnc-bridge",
        "version": args.version,
        "board": args.board,
        "built": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "flash": {
            "size": settings["flash_size"],
            "mode": settings["flash_mode"],
            "freq": settings["flash_freq"],
        },
        "builds": [{"chipFamily": "ESP32", "parts": parts}],
    }
    out = args.out_dir / f"{prefix}-manifest.json"
    out.write_text(json.dumps(manifest, indent=2) + "\n")
    for p in parts:
        print(f"{p['offset']:#08x}  {p['size']:>8}  {p['path']}")
    print(out)


if __name__ == "__main__":
    main()
