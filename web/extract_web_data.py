#!/usr/bin/env python3
"""Extract WTVPACK2 bundle into web-ready static data directory (raw images + pre-gzipped assets)
or strip the Windows PE prefix into a standalone .wtvpack file for fast server upload.
"""

from __future__ import annotations

import argparse
import gzip
import json
import lzma
import struct
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

MAGIC = b"WTVPACK2"
RAW_EXTS = {".jpg", ".jpeg", ".png", ".webp"}


def read_pack_index(pack_path: Path) -> tuple[list[list], int]:
    with open(pack_path, "rb") as f:
        f.seek(0, 2)
        size = f.tell()
        if size < 24:
            raise RuntimeError(f"{pack_path} too small to contain WTVPACK2")
        f.seek(size - 24)
        idx_off, idx_len = struct.unpack("<QQ", f.read(16))
        magic = f.read(8)
        if magic != MAGIC or idx_off + idx_len + 24 != size:
            raise RuntimeError(f"{pack_path} does not have a valid WTVPACK2 footer")
        f.seek(idx_off)
        entries = json.loads(f.read(idx_len).decode("utf-8"))
        return entries, idx_off


def strip_to_pack(src_exe: Path, out_pack: Path) -> None:
    t0 = time.time()
    entries, _ = read_pack_index(src_exe)
    first_off = min(e[1] for e in entries)
    out_pack.parent.mkdir(parents=True, exist_ok=True)
    tmp = out_pack.with_suffix(".partial")
    new_entries = []
    with open(src_exe, "rb") as rf, open(tmp, "wb") as wf:
        wf.write(MAGIC)  # 8-byte header
        pos = 8
        for rel, off, length, codec in entries:
            rf.seek(off)
            chunk = rf.read(length)
            wf.write(chunk)
            new_entries.append([rel, pos, length, codec])
            pos += length
        idx_bytes = json.dumps(new_entries, separators=(",", ":")).encode("utf-8")
        wf.write(idx_bytes)
        wf.write(struct.pack("<QQ", pos, len(idx_bytes)))
        wf.write(MAGIC)
    if out_pack.exists():
        out_pack.unlink()
    tmp.rename(out_pack)
    mb = out_pack.stat().st_size / (1024 * 1024)
    print(f"Created standalone pack {out_pack} ({len(new_entries)} entries, {mb:.1f} MB, stripped {first_off/1048576:.1f} MB exe stub) in {time.time()-t0:.1f}s")


def unpack_one(pack_path: Path, out_dir: Path, entry: list) -> tuple[str, int]:
    rel, off, length, codec = entry
    if codec != 1:
        raise RuntimeError(f"Unsupported codec {codec} for {rel}")
    if "\\" in rel or ".." in rel.split("/"):
        raise RuntimeError(f"Unsafe relative path in pack: {rel}")
    target = out_dir / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    with open(pack_path, "rb") as f:
        f.seek(off)
        xz_bytes = f.read(length)
    raw = lzma.decompress(xz_bytes)
    if target.suffix.lower() in RAW_EXTS:
        target.write_bytes(raw)
        return rel, len(raw)
    gz_target = Path(str(target) + ".gz")
    gz_bytes = gzip.compress(raw, compresslevel=6, mtime=0)
    gz_target.write_bytes(gz_bytes)
    return rel, len(gz_bytes)


def unpack_pack(pack_path: Path, out_dir: Path, only_prefix: list[str] | None = None, workers: int = 4) -> None:
    t0 = time.time()
    entries, _ = read_pack_index(pack_path)
    if only_prefix:
        entries = [
            e for e in entries
            if e[0] in ("index.js", "helipads.js") or e[0].startswith("tanks/") or any(e[0].startswith(p) for p in only_prefix)
        ]
    out_dir.mkdir(parents=True, exist_ok=True)
    total_bytes = 0
    done = 0
    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        futs = [pool.submit(unpack_one, pack_path, out_dir, e) for e in entries]
        for fut in as_completed(futs):
            _, sz = fut.result()
            total_bytes += sz
            done += 1
            if done % 50 == 0 or done == len(entries):
                print(f"  unpacked {done}/{len(entries)} ({total_bytes/1048576:.1f} MB written)...", flush=True)
    print(f"Unpacked {done} entries to {out_dir} ({total_bytes/1048576:.1f} MB on disk) in {time.time()-t0:.1f}s")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--src", required=True, help="Path to 婴儿弹道计算机.exe or .wtvpack")
    ap.add_argument("--strip-out", help="Write standalone .wtvpack without .exe stub")
    ap.add_argument("--unpack-to", help="Unpack web-ready static data into this folder")
    ap.add_argument("--prefix", action="append", help="Optional prefix filter (e.g. maps/avg_abandoned_factory)")
    ap.add_argument("--workers", type=int, default=4)
    args = ap.parse_args()

    src = Path(args.src)
    if args.strip_out:
        strip_to_pack(src, Path(args.strip_out))
    if args.unpack_to:
        unpack_pack(src, Path(args.unpack_to), only_prefix=args.prefix, workers=args.workers)


if __name__ == "__main__":
    main()
