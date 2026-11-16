#!/usr/bin/env python3
"""Extract War Thunder ground maps, mission areas and tank shell ballistics into ../data for the app.

Reads the installed game with wt-unpack.exe (vromfs, BLK -> JSON, HM2 heightmap -> 16-bit PNG).
Writes ../data/index.js and ../data/maps/<level>.js (loaded by <script> tags, so file:// works offline).

Usage:  python extract.py [--game "D:\\...\\War Thunder"] [--levels avg_abandoned_factory,avg_berlin]
Needs:  Pillow, numpy.
"""
import argparse
import base64
import csv
import io
import json
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

import numpy as np
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_GAME = r"D:\SteamLibrary\steamapps\common\War Thunder"
MAX_GRID = 4096  # heightmaps above this are strided down to keep the browser under ~100 MB


def find_wt_unpack(arg):
    if arg:
        return Path(arg)
    hits = sorted(ROOT.glob("wt-unpack*/wt-unpack.exe"))
    if not hits:
        sys.exit("wt-unpack.exe not found next to the project; pass --wt-unpack")
    return hits[-1]


def run(exe, *args):
    r = subprocess.run([str(exe), *map(str, args)], capture_output=True, text=True, encoding="utf-8", errors="replace")
    if r.returncode != 0:
        raise RuntimeError(f"wt-unpack {' '.join(map(str, args))} failed:\n{r.stderr[-2000:]}")
    return r.stdout


def load_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def as_list(v):
    return v if isinstance(v, list) else ([] if v is None else [v])


def iter_dicts(o):
    if isinstance(o, dict):
        yield o
        for v in o.values():
            yield from iter_dicts(v)
    elif isinstance(o, list):
        for v in o:
            yield from iter_dicts(v)


# ---------------------------------------------------------------- lang

def load_lang(lang_dir, col_name="<Chinese>"):
    names = {}
    for fn in ("units.csv", "units_weaponry.csv", "missions_locations.csv", "missions_versus.csv"):
        p = lang_dir / "lang" / fn
        if not p.exists():
            continue
        with open(p, encoding="utf-8-sig", newline="") as f:
            r = csv.reader(f, delimiter=";", quotechar='"')
            hdr = next(r, [])
            ci = hdr.index(col_name) if col_name in hdr else 1
            for row in r:
                if len(row) > ci and row[0] and not row[0].startswith("<"):
                    val = (row[ci] or (row[1] if len(row) > 1 else "")).replace("\u200b", "").replace("\xa0", " ").strip()
                    if val:
                        names.setdefault(row[0].lower(), val)
    return names


# ---------------------------------------------------------------- tanks

def first(v):
    # a BLK key repeated in one block becomes a JSON list; Dagor's getReal() returns the first one
    return v[0] if isinstance(v, list) and v else v


def shell_from_bullet(b, ammo_key):
    # Cx may be missing (many WW2 guns): the game then uses a built-in default we cannot read; see estimate_cx()
    if not isinstance(b, dict) or "speed" not in b or "mass" not in b:
        return None
    b = {k: first(b[k]) for k in ("speed", "mass", "Cx", "caliber", "ballisticCaliber", "maxDistance", "bulletName", "bulletType") if k in b}
    cal = b.get("ballisticCaliber") or b.get("caliber")
    if not isinstance(cal, (int, float)) or not cal or b["mass"] <= 0 or b["speed"] <= 0:
        return None
    return {
        "ammo": ammo_key,
        "id": b.get("bulletName", ammo_key),
        "type": b.get("bulletType", "?"),
        "v0": round(float(b["speed"]), 3),
        "m": float(b["mass"]),
        "cal": float(cal),  # drag reference diameter (ballisticCaliber for sub-calibre rounds)
        "cx": float(b["Cx"]) if isinstance(b.get("Cx"), (int, float)) else None,
        "maxDist": float(b.get("maxDistance", 0) or 0),
    }


def estimate_cx(tanks):
    """Fill shells without Cx with the median Cx of shells of the same bulletType (else the same type family,
    e.g. "apcbc", else all shells) and mark them cxEst. The game's own default is not in the data files."""
    by_type, by_family, everything = {}, {}, []
    shells = [s for t in tanks for g in t["guns"] for s in g["shells"]]
    for s in shells:
        if s["cx"] is not None:
            by_type.setdefault(s["type"], []).append(s["cx"])
            by_family.setdefault(s["type"].split("_")[0], []).append(s["cx"])
            everything.append(s["cx"])
    for s in shells:
        if s["cx"] is None:
            pool = by_type.get(s["type"]) or by_family.get(s["type"].split("_")[0]) or everything
            s["cx"], s["cxEst"] = round(float(np.median(pool)), 4), True


def shells_of_weapon(w):
    """Default bullet plus every named ammo block that holds a bullet (or a belt of them)."""
    out, seen = [], set()
    groups = [("default", w.get("bullet"))] + [(k, v.get("bullet")) for k, v in w.items() if isinstance(v, dict) and "bullet" in v]
    for key, bl in groups:
        for b in as_list(bl):
            s = shell_from_bullet(b, key)
            if s and (s["id"], s["v0"], s["cx"]) not in seen and not b.get("rocket"):
                seen.add((s["id"], s["v0"], s["cx"]))
                out.append(s)
    return out


def extract_tanks(aces, names):
    tank_dir = aces / "gamedata" / "units" / "tankmodels"
    weapon_cache = {}
    tanks = []
    for f in sorted(tank_dir.glob("*.blk.json")):
        tid = f.name[: -len(".blk.json")]
        title = names.get(tid + "_shop") or names.get(tid + "_0")
        if not title:
            continue  # AI/dummy units have no shop name
        t = load_json(f)
        sources = [t]
        for d in iter_dicts(t.get("weapon_presets", {})):
            p = d.get("blk")
            if isinstance(p, str) and p.lower().endswith(".blk"):
                pp = aces / (p.lower() + ".json")
                if pp.exists():
                    sources.append(load_json(pp))
        cannons, others, seen_blk = [], [], set()
        for src in sources:
            for d in iter_dicts(src):
                p = d.get("blk")
                if not isinstance(p, str) or "groundmodels_weapons" not in p.lower() or p.lower() in seen_blk:
                    continue
                seen_blk.add(p.lower())
                wp = aces / (p.lower() + ".json")
                if wp not in weapon_cache:
                    weapon_cache[wp] = load_json(wp) if wp.exists() else None
                w = weapon_cache[wp]
                if not w:
                    continue
                shells = shells_of_weapon(w)
                if not shells:
                    continue
                pitch = (d.get("limits") or {}).get("pitch") or [-10.0, 20.0]
                for s in shells:
                    s["name"] = names.get(s["id"].lower(), s["id"])
                (cannons if w.get("cannon") else others).append({"weapon": Path(p).stem, "pitch": [float(pitch[0]), float(pitch[1])], "shells": shells})
        # machine-gun-class weapons (autocannons of SPAA, 20 mm light tanks) only for vehicles without a cannon
        guns = cannons or others
        if guns:
            model = t.get("model")
            tanks.append({"id": tid, "name": title, "model": model.lower() if isinstance(model, str) else None, "guns": guns})
    estimate_cx(tanks)
    return tanks


# ---------------------------------------------------------------- missions / areas

def resolve_mission(path, mis_root, depth=0):
    """Areas and units of a mission merged with everything it imports (imports first, own entries win)."""
    d = load_json(path)
    areas, units = {}, []
    trig = d.get("triggers") or {}
    # The ground battle area: the area of a missionBattleArea action for ground units with killOutOfBattleArea
    # (leaving it starts the countdown; the kill-area actions are detection areas without that flag).
    # Its `area` is a name or "@variable"; _vars holds every string value a varSetString gives each variable.
    d["_battle_refs"] = [dd.get("area") or dd.get("target") for t in iter_dicts(trig)
                         for k in ("missionBattleArea", "missionMarkAsBattleArea") for dd in as_list(t.get(k))
                         if isinstance(dd, dict) and dd.get("ground") in (True, "yes") and dd.get("killOutOfBattleArea") in (True, "yes")]
    # kill areas (prohibited zones): missionBattleArea actions with killArea for ground units
    d["_kill_refs"] = [dd.get("area") or dd.get("target") for t in iter_dicts(trig) for dd in as_list(t.get("missionBattleArea"))
                       if isinstance(dd, dict) and dd.get("ground") in (True, "yes") and dd.get("killArea") in (True, "yes")]
    # variables: defaults from the `variables` block (battlearea_exclude_template sets its squad name there)
    d["_vars"] = {k: [v] for k, v in (d.get("variables") or {}).items() if isinstance(v, str)}
    for t in iter_dicts(trig):
        for dd in as_list(t.get("varSetString")):
            if isinstance(dd, dict) and isinstance(dd.get("value"), str):
                for v in as_list(dd.get("var")):
                    d["_vars"].setdefault(v, []).append(dd["value"])
    if depth < 6:
        for rec in as_list((d.get("imports") or {}).get("import_record")):
            fp = rec.get("file") if isinstance(rec, dict) else None
            if fp:
                ip = mis_root / (fp.lower() + ".json")
                if ip.exists():
                    a, u, sub = resolve_mission(ip, mis_root, depth + 1)
                    areas.update(a)
                    units += u
                    d["_battle_refs"] += sub["_battle_refs"]
                    d["_kill_refs"] += sub["_kill_refs"]
                    for v, vals in sub["_vars"].items():
                        d["_vars"].setdefault(v, []).extend(vals)
    for k, v in (d.get("areas") or {}).items():
        for a in as_list(v):
            if isinstance(a, dict) and "tm" in a:
                areas[k] = a
    for kind, v in (d.get("units") or {}).items():
        for u in as_list(v):
            if isinstance(u, dict):
                units.append((kind, u))
    return areas, units, d


def team_of(name):
    n = name.lower()
    return 1 if n.startswith("t1") or "_t1_" in n else 2 if n.startswith("t2") or "_t2_" in n else 0


MODE_SUFFIX = {"arcade": "arcade", "realistic": "realistic", "simulator": "hardcore"}


def area_mode(name):
    """Game mode an area variant is for, from the suffix the mode templates append (simulator = _hardcore)."""
    n = name.lower()
    return next((m for m, s in MODE_SUFFIX.items() if n.endswith("_" + s)), None)


def mission_areas(path, mis_root):
    areas, units, d = resolve_mission(path, mis_root)
    ms = d.get("mission_settings") or {}
    picked = {}  # name -> kind

    def add(name, kind):
        if name in areas and name not in picked:
            picked[name] = kind

    # 1. what the ground missionBattleArea trigger uses (killOutOfBattleArea: the countdown border). A "@var"
    #    area is a base name the template extends by game mode (conq_battle_area + _arcade/_realistic/_hardcore),
    #    so every area named base or base_<suffix> is kept, tagged by mode (see area_mode)
    # area_squad units by name: an action's area can be a squad (battlearea_exclude_squad), meaning all its members
    squads = {str(u.get("name", "")): as_list((u.get("props") or {}).get("squad_members"))
              for kind, u in units if kind == "area_squad"}

    def add_refs(refs, kind):
        for ref in refs:
            if not isinstance(ref, str):
                continue
            bases = d["_vars"].get(ref[1:], []) if ref.startswith("@") else [ref]
            for b in bases:
                for m in squads.get(b, []):
                    add(m, kind)
                for n in areas:
                    if n == b or n.startswith(b + "_"):
                        add(n, kind)
    add_refs(d["_kill_refs"], "kill")  # first: kill areas are named like t1_killarea, never like battle areas
    add_refs(d["_battle_refs"], "battle")
    # 2. the briefing's battle area (what the briefing map draws), when no trigger names one
    brief = ms.get("briefing") or {}
    for dd in iter_dicts(brief):
        if "battleArea" in dd and not any(k == "battle" for k in picked.values()):
            for ba in as_list(dd["battleArea"]):
                if isinstance(ba, dict):
                    add(ba.get("target"), "battle")
        it = dd.get("icontype")
        if isinstance(it, str) and "zone" in it.lower():
            add(dd.get("target"), "capture")
    # 3. an area named like a battle area (some templates build the name in ways the variables above miss)
    if not any(k == "battle" for k in picked.values()):
        for n in areas:
            if "battle_area" in n.lower():
                add(n, "battle")
    for kind, u in units:
        if kind != "area_squad":
            continue
        n = str(u.get("name", "")).lower()
        k = "kill" if "killarea" in n else "spawn" if "spawn" in n and "tank" in n else None
        if k:
            for m in as_list((u.get("props") or {}).get("squad_members")):
                add(m, k)
    out = []
    for name, kind in picked.items():
        a = areas[name]
        rec = {"name": name, "kind": kind, "team": team_of(name), "type": a.get("type", "?"),
               "tm": [[round(x, 3) for x in row] for row in a["tm"]]}
        mode = area_mode(name)
        if mode:
            rec["mode"], rec["group"] = mode, name[: name.lower().rindex("_" + MODE_SUFFIX[mode])]
        out.append(rec)
    mission = ms.get("mission") or {}
    return str(mission.get("level", "")), str(mission.get("type", "")), out


def extract_missions(mis):
    root = mis / "gamedata" / "missions" / "cta" / "tanks"
    by_level = {}
    for f in sorted(root.rglob("*.blk.json")):
        if f.name.startswith("template_"):
            continue  # imported building blocks; some still name another map's level
        try:
            level, mtype, areas = mission_areas(f, mis)
        except (KeyError, TypeError, ValueError, AttributeError) as e:
            print(f"  skip mission {f.name}: {e}", file=sys.stderr)
            continue
        if not level.startswith("levels/") or not areas:
            continue
        lv = Path(level).stem
        by_level.setdefault(lv, []).append({"name": f.name[: -len(".blk.json")], "type": mtype, "areas": areas})
    return by_level


# ---------------------------------------------------------------- maps

def level_settings(exe, game, level):
    """levels/<level>.blk as text (from aces.vromfs.bin), or "" when the level has none."""
    try:
        return run(exe, "cat", f"{Path(game) / 'aces.vromfs.bin'}:levels/{level}.blk")
    except RuntimeError:
        return ""


def map_bounds(text):
    """[x0, z0, x1, z1] of the tank map from the level settings (tankMapCoord0/1, else mapCoord0/1), or None.
    mapCoord is the air map and spans tens of km on tank levels, so tankMapCoord comes first."""
    for key in ("tankMapCoord", "mapCoord"):
        pts = [re.search(rf"^\s*{key}{i}:p2\s*=\s*([-\d.eE+]+)\s*,\s*([-\d.eE+]+)", text, re.M) for i in (0, 1)]
        if all(pts):
            (x0, z0), (x1, z1) = [(float(p.group(1)), float(p.group(2))) for p in pts]
            return [min(x0, x1), min(z0, z1), max(x0, x1), max(z0, z1)]
    return None


def water_level(text, hmin):
    """The level's water plane height (top-level water_level:r), or None when it has none or no ground lies below it."""
    m = re.search(r"^water_level:r\s*=\s*([-\d.eE+]+)", text, re.M)
    return float(m.group(1)) if m and float(m.group(1)) > hmin else None


def extract_map(exe, game, level, missions, tmp, out_dir):
    png = tmp / f"{level}.png"
    run(exe, "level", Path(game) / "levels" / f"{level}.bin", "--heightmap", png)
    meta = load_json(png.with_suffix(".json"))
    with Image.open(png) as im:
        h = np.asarray(im).astype(np.uint16)
    assert h.shape == (meta["height"], meta["width"]), (h.shape, meta)
    cell = float(meta["cell_size"])
    stride = 1
    while max(h.shape) // stride > MAX_GRID:
        stride *= 2
    if stride > 1:
        h = np.ascontiguousarray(h[::stride, ::stride])  # shortcut: strided, not filtered; fine for >8 km landmesh maps
        cell *= stride
    blob = base64.b64encode(zlib.compress(h.astype("<u2").tobytes(), 6)).decode("ascii")
    settings = level_settings(exe, game, level)
    info = {
        "name": level, "bounds": map_bounds(settings), "width": int(h.shape[1]), "height": int(h.shape[0]), "cell": cell,
        "ox": float(meta["origin_x"]), "oz": float(meta["origin_z"]),
        "hmin": float(meta["height_min"]), "hmax": float(meta["height_max"]),
        "source": meta.get("source", "?"), "missions": missions,
        "water": water_level(settings, float(meta["height_min"])),
    }
    js = "WTV_MAP(" + json.dumps(info, separators=(",", ":"))[:-1] + ',"h":"' + blob + '"});\n'
    (out_dir / f"{level}.js").write_text(js, encoding="ascii")
    return {k: info[k] for k in ("name", "width", "height", "cell", "ox", "oz", "source")} | {"missions": len(missions)}


def keep_objects(maps, out_dir):
    """--skip-objects: keep the object and ground files of an earlier run (same heightmap grid)."""
    for m in maps:
        m["objects"] = (out_dir / f"{m['name']}.obj.js").exists()
        m["ground"] = (out_dir / f"{m['name']}.tex.js").exists()


OBJECTS_EXE = Path(__file__).resolve().parent / "objects" / "target" / "release" / "wtv-objects.exe"


def extract_objects(game, maps, tmp, out_dir):
    """Objects, trees and ground colours via the Rust exporter in objects/:
    <level>.obj.js (objects, trees, collision grids) and <level>.tex.js (ground colour JPEG) per map."""
    if not OBJECTS_EXE.exists():
        print(f"no objects or ground colours: {OBJECTS_EXE} is not built (see README)", file=sys.stderr)
        return
    print("exporting objects, trees and ground colours ...")
    jobs = tmp / "objects_jobs.txt"
    # shortcut: the exporter splits job lines on whitespace, so tmp must not contain spaces (tempfile's dir has none)
    jobs.write_text("".join(f"{m['name']} {tmp / m['name']} {m['ox']} {m['oz']} {m['cell']} {m['width']} {m['height']}\n" for m in maps))
    r = subprocess.run([str(OBJECTS_EXE), str(game), str(jobs)], capture_output=True, text=True, encoding="utf-8", errors="replace")
    print(r.stdout, end="")
    if r.returncode:
        print(r.stderr.strip().splitlines()[-1] if r.stderr.strip() else "wtv-objects failed", file=sys.stderr)
    for m in maps:
        src = tmp / (m["name"] + ".obj")
        if src.exists():
            blob = base64.b64encode(zlib.compress(src.read_bytes(), 6)).decode("ascii")
            (out_dir / f"{m['name']}.obj.js").write_text(f'WTV_OBJ("{m["name"]}","{blob}");\n', encoding="ascii")
            m["objects"] = True
        ground = tmp / (m["name"] + ".ground")
        if ground.exists():
            raw = ground.read_bytes()
            w, h = struct.unpack("<II", raw[:8])
            o = 8 + w * h * 4

            def data_url(im, fmt, **kw):
                buf = io.BytesIO()
                im.save(buf, fmt, **kw)
                return f"data:image/{fmt.lower()};base64,{base64.b64encode(buf.getvalue()).decode('ascii')}"
            # row 0 is the smallest z; the page draws +Z up, like the heightmap shading
            colour = data_url(ImageOps.flip(Image.frombytes("RGBA", (w, h), raw[8:o]).convert("RGB")), "JPEG", quality=85)
            detail = None
            if len(raw) > o:  # detail index per texel + the detail textures, side by side in one strip
                idx = ImageOps.flip(Image.frombytes("L", (w, h), raw[o:o + w * h]))
                o += w * h
                n, = struct.unpack_from("<I", raw, o)
                o += 4
                tiles, strip = [], None
                for i in range(n):
                    t, side = struct.unpack_from("<fI", raw, o)
                    strip = strip or Image.new("L", (side * n, side))
                    strip.paste(Image.frombytes("L", (side, side), raw[o + 8:o + 8 + side * side]), (side * i, 0))
                    tiles.append(round(t, 2))
                    o += 8 + side * side
                if n:
                    detail = {"idx": data_url(idx, "PNG", optimize=True), "strip": data_url(strip, "PNG", optimize=True), "tiles": tiles}
            (out_dir / f"{m['name']}.tex.js").write_text(
                f'WTV_TEX("{m["name"]}","{colour}",{json.dumps(detail)});\n', encoding="ascii")
            m["ground"] = True


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--game", default=DEFAULT_GAME)
    ap.add_argument("--wt-unpack")
    ap.add_argument("--levels", help="comma-separated level names (default: every level used by a tank mission)")
    ap.add_argument("--out", default=str(ROOT / "data"))
    ap.add_argument("--keep-tmp", action="store_true")
    ap.add_argument("--skip-objects", action="store_true", help="keep the objects/trees/ground files of an earlier run")
    ap.add_argument("--lang", default="<Chinese>", help="CSV language column header, e.g. <Chinese> or <English>")
    a = ap.parse_args()

    exe = find_wt_unpack(a.wt_unpack)
    game = Path(a.game)
    out = Path(a.out)
    (out / "maps").mkdir(parents=True, exist_ok=True)
    tmp = Path(tempfile.mkdtemp(prefix="wtv_"))
    try:
        print("unpacking archives ...")
        run(exe, "extract", game / "aces.vromfs.bin", "-o", tmp / "aces", "--blk", "json", "--filter", "tankmodels")
        run(exe, "extract", game / "aces.vromfs.bin", "-o", tmp / "aces", "--blk", "json", "--filter", "groundmodels_weapons")
        run(exe, "extract", game / "mis.vromfs.bin", "-o", tmp / "mis", "--blk", "json", "--filter", "gamedata/missions")
        run(exe, "extract", game / "lang.vromfs.bin", "-o", tmp / "lang", "--filter", "lang/")

        names = load_lang(tmp / "lang", a.lang)
        tanks = extract_tanks(tmp / "aces", names)
        print(f"tanks: {len(tanks)}")
        by_level = extract_missions(tmp / "mis")
        # "water" is the empty ocean level that many tank test/event missions point at, not a battlefield
        levels = a.levels.split(",") if a.levels else sorted(set(by_level) - {"water"})
        maps = []
        for i, lv in enumerate(levels, 1):
            try:
                m = extract_map(exe, game, lv, by_level.get(lv, []), tmp, out / "maps")
                maps.append(m)
                print(f"[{i}/{len(levels)}] {lv}: {m['width']}x{m['height']} @ {m['cell']} m ({m['source']}), {m['missions']} missions")
            except (RuntimeError, OSError, AssertionError, KeyError) as e:
                print(f"[{i}/{len(levels)}] {lv}: FAILED {str(e).splitlines()[0]}", file=sys.stderr)
        if a.skip_objects:
            keep_objects(maps, out / "maps")
        else:
            extract_objects(game, maps, tmp, out / "maps")
        index = {"maps": maps, "tanks": tanks}
        (out / "index.js").write_text("WTV_INDEX(" + json.dumps(index, separators=(",", ":")) + ");\n", encoding="utf-8")
        print(f"wrote {out / 'index.js'} ({len(maps)} maps)")
    finally:
        if a.keep_tmp:
            print(f"kept {tmp}")
        else:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
