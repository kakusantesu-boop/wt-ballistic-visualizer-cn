#!/usr/bin/env python3
"""Convert 56 exported .air.raw outer-airspace files into .air.jpg + .air.hpred,
pack the 6 high-precision 3D helipad airfield prefabs into helipads.js,
and inject `air` + true ground-snapped helipad elevations into each i18n_src/patched_js/maps/<lv>.js.
"""

from __future__ import annotations

import base64
import json
import struct
import time
import zlib
from pathlib import Path

import numpy as np
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parent.parent
RAW_DIR = ROOT / "i18n_src" / "airspace_raw"
PREFAB_DIR = RAW_DIR / "helipad_prefabs"
PATCHED_DIR = ROOT / "i18n_src" / "patched_js"
MAPS_DIR = PATCHED_DIR / "maps"


def predict_u16(h: np.ndarray) -> bytes:
    """Exact vectorised match of rust/src/store.rs::predict(b, w):
    for row=0, x>0: l=h[0,x-1], u=0, ul=0  (note: store.rs uses `i > w` so i==w has ul=0!)
    """
    h32 = h.astype(np.int32)
    flat = h32.ravel()
    w = h.shape[1]
    n = flat.size
    idx = np.arange(n)
    x = idx % w
    l = np.where(x > 0, np.roll(flat, 1), 0)
    u = np.where(idx >= w, np.roll(flat, w), 0)
    ul = np.where((x > 0) & (idx > w), np.roll(flat, w + 1), 0)
    res = (flat - (l + u - ul)) & 0xFFFF
    return res.astype("<u2").tobytes()


def pack_helipads_js() -> dict:
    wtvo_path = PREFAB_DIR / "models.wtvo"
    prefabs_path = PREFAB_DIR / "prefabs.json"
    if not wtvo_path.exists() or not prefabs_path.exists():
        print("[WARN] helipad_prefabs not found; skipping helipads.js")
        return {}
    wtvo_bytes = wtvo_path.read_bytes()
    b64_models = base64.b64encode(zlib.compress(wtvo_bytes, level=9)).decode("ascii")
    prefabs_raw = json.loads(prefabs_path.read_text(encoding="utf-8"))
    # Round instance transform floats cleanly to 4 decimals for compact JSON
    compact_prefabs = {}
    for uc, pf in prefabs_raw.items():
        inst_list = []
        for mid, tm in pf["inst"]:
            inst_list.append([int(mid)] + [round(float(x), 4) for x in tm])
        compact_prefabs[uc] = {
            "hx": round(float(pf["hx"]), 1),
            "hz": round(float(pf["hz"]), 1),
            "rwy": round(float(pf["runway_half_len"]), 1),
            "spawns": [[round(float(s[0]), 1), round(float(s[1]), 1)] for s in pf["spawns"]],
            "inst": inst_list,
        }
    payload = {"models": b64_models, "prefabs": compact_prefabs}
    out_js = PATCHED_DIR / "helipads.js"
    out_js.write_text(
        "WTV_PADS(" + json.dumps(payload, ensure_ascii=False, separators=(",", ":")) + ");\n",
        encoding="utf-8",
    )
    print(f"Packed {out_js.name}: {out_js.stat().st_size / 1024:.1f} KB ({len(compact_prefabs)} prefabs)")
    return compact_prefabs


def main() -> None:
    t0 = time.time()
    compact_prefabs = pack_helipads_js()
    true_y_path = RAW_DIR / "helipad_true_y.json"
    true_y_all = json.loads(true_y_path.read_text(encoding="utf-8")) if true_y_path.exists() else {}

    raw_files = sorted(RAW_DIR.glob("avg_*.air.raw"))
    assert len(raw_files) == 56, f"Expected 56 .air.raw files in {RAW_DIR}, found {len(raw_files)}"

    total_jpg = 0
    total_hp = 0
    meta_map = {}

    for rf in raw_files:
        lv = rf.name[: -len(".air.raw")]
        buf = rf.read_bytes()
        magic, w, h, ox, oz, cell, hmin, hmax = struct.unpack_from("<4sIIfffff", buf, 0)
        assert magic == b"WTVA", f"Bad magic in {rf}"
        u16_off = 32
        u16_end = u16_off + w * h * 2
        rgb_end = u16_end + w * h * 3
        assert len(buf) == rgb_end, f"Size mismatch in {rf}"

        h_arr = np.frombuffer(buf, dtype="<u2", count=w * h, offset=u16_off).reshape((h, w))
        hpred = predict_u16(h_arr)
        hp_path = MAPS_DIR / f"{lv}.air.hpred"
        hp_path.write_bytes(hpred)
        total_hp += len(hpred)

        # Row 0 of GroundBaker::bake is smallest Z; flip vertically so row 0 = largest Z (+Z up)
        im = ImageOps.flip(Image.frombytes("RGB", (w, h), buf[u16_end:rgb_end]))
        jpg_path = MAPS_DIR / f"{lv}.air.jpg"
        im.save(jpg_path, "JPEG", quality=84, optimize=True)
        total_jpg += jpg_path.stat().st_size

        air_meta = {
            "width": int(w),
            "height": int(h),
            "cell": round(float(cell), 3),
            "ox": round(float(ox), 3),
            "oz": round(float(oz), 3),
            "hmin": round(float(hmin), 3),
            "hmax": round(float(hmax), 3),
            "h": f"@maps/{lv}.air.hpred",
            "tex": f"@maps/{lv}.air.jpg",
        }
        meta_map[lv] = air_meta

        js_path = MAPS_DIR / f"{lv}.js"
        s = js_path.read_text(encoding="utf-8").strip()
        assert s.startswith("WTV_MAP(") and s.endswith(");")
        obj = json.loads(s[len("WTV_MAP(") : -2])
        obj["air"] = air_meta
        ty_map = true_y_all.get(lv) or {}
        for p in obj.get("helipads", []):
            if p["id"] in ty_map:
                p["y"] = round(float(ty_map[p["id"]]), 2)
            uc_low = str(p.get("uc", "")).lower()
            if uc_low in compact_prefabs:
                p["hx"] = compact_prefabs[uc_low]["hx"]
                p["hz"] = compact_prefabs[uc_low]["hz"]
        js_path.write_text("WTV_MAP(" + json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + ");\n", encoding="utf-8")

    (RAW_DIR / "air_meta_index.json").write_text(json.dumps(meta_map, indent=2), encoding="utf-8")
    print(
        f"Packed {len(raw_files)} outer airspace maps in {time.time() - t0:.1f}s "
        f"(JPEG={total_jpg / 1048576:.1f} MB, raw hpred={total_hp / 1048576:.1f} MB)"
    )


if __name__ == "__main__":
    main()
