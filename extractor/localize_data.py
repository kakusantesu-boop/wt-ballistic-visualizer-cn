import csv
import json
import lzma
import math
import re
import struct
from pathlib import Path
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
LANG_DIR = ROOT / "i18n_src"
DM_ACES = LANG_DIR / "wt-datamine" / "aces.vromfs.bin_u"
DM_MIS = LANG_DIR / "wt-datamine" / "mis.vromfs.bin_u"
DM_CHAR = LANG_DIR / "wt-datamine" / "char.vromfs.bin_u"
TANK_DIR = DM_ACES / "gamedata" / "units" / "tankmodels"
WEAP_DIR = DM_ACES / "gamedata" / "weapons" / "groundmodels_weapons"
AIR_WEAP_DIR = DM_ACES / "gamedata" / "weapons"
FM_DIR = DM_ACES / "gamedata" / "flightmodels"
LITE_EXE = ROOT / "dist" / "WT-Ballistic-Visualizer-Lite.exe"

LITE_A_MODEL = "leopard_2a4"

NATION_TAGS = {
    "us": "美",
    "usa": "美",
    "germ": "德",
    "germany": "德",
    "ussr": "苏",
    "uk": "英",
    "britain": "英",
    "jp": "日",
    "japan": "日",
    "cn": "中",
    "china": "中",
    "it": "意",
    "italy": "意",
    "fr": "法",
    "france": "法",
    "sw": "瑞",
    "sweden": "瑞",
    "il": "以",
    "israel": "以",
}

SUBTREE_GLYPHS = {
    "\u2417": "中系外购/援助",
    "\u2419": "电竞杯赛",
    "\u2420": "猛兽特别版",
    "\u2580": "缴获",
    "\u2582": "租借/缴获",
    "\u2583": "特殊/外购",
    "\u2584": "外购/援助",
    "\u2585": "自卫队美械",
    "\u25c4": "西德",
    "\u25c6": "纪念版",
    "\u25ca": "东德",
    "\u25cb": "新兵礼包",
    "\u25cc": "瑞士",
    "\u25cd": "高级礼包",
    "\u25d0": "希腊",
    "\u25d4": "匈牙利",
    "\u25d7": "荷兰",
    "\u25d8": "荷属",
    "\u25e1": "哥伦比亚",
    "\u25e2": "挪威",
    "\u25e5": "印尼",
    "\uf059": "以军",
}

# Iconic War Thunder Wiki / community Chinese nicknames appended or searchable
WIKI_DISPLAY_NICKNAMES = {
    "ussr_bm_31_12": "BM-31-12（安德柳沙）",
    "ussr_bm_13n": "BM-13N（喀秋莎）",
}

WIKI_EXTRA_SEARCH = {
    "ussr_bm_31_12": "安德柳沙 andryusha bm3112 bm31-12 喀秋莎火箭炮",
    "ussr_bm_13n": "喀秋莎 katyusha bm13n 火箭炮",
    "ussr_bm_8_24": "小喀秋莎 katyusha bm824 火箭炮",
    "germ_sturmmorser_sturmtiger": "突击虎 暴风虎 sturmtiger 380mm 臼炮",
    "germ_sturmmorser_sturmtiger_event": "突击虎 暴风虎 sturmtiger 380mm 臼炮",
    "us_m4_sherman_calliope": "管风琴 谢尔曼 calliope 火箭炮",
    "jp_type_75_mlrs": "75式火箭炮 mlrs 多管火箭炮",
    "germ_panzerwerfer_42": "42式火箭炮 装甲火箭发射车 panzerwerfer nebelwerfer",
    "germ_spz_oerlikon_raketenautomat": "自动火箭发射车 raketenautomat",
    "uk_churchill_avre": "丘吉尔avre 佩塔德臼炮 petard 飞天垃圾桶",
    "uk_centurion_mk_5_avre_era": "百夫长avre 165mm 破障炮",
    "uk_matilda_hedgehog": "玛蒂尔达刺猬 刺猬弹 hedgehog",
    "ussr_rbt_5": "rbt5 坦克鱼雷 420mm",
    "ussr_2s38": "2s38 衍生品 57神车",
    "ussr_pantsyr_s1": "铠甲s1 pantsir",
}

WINTER_MAP_OVERRIDES = {
    "avg_ardennes_snow": ("Ardennes (Winter)", "阿登 • 冬季"),
    "avg_ardennes": ("Ardennes", "阿登（突出部）"),
    "avg_poland_snow": ("Poland (Winter)", "波兰 • 冬季"),
    "avg_sector_montmedy_snow": ("Maginot Line (Winter)", "马其诺防线 • 冬季"),
    "avg_soviet_suburban_snow": ("Seversk-13 (Winter)", "谢韦尔斯克-13 • 冬季"),
}

MISSION_MODE_LABELS = {
    "dom": "【战区控制】",
    "bttl": "【战区攻陷】",
    "conq": "【战区争夺】",
    "conq1": "【战区争夺】#1",
    "conq2": "【战区争夺】#2",
    "conq3": "【战区争夺】#3",
    "conq4": "【战区争夺】#4",
    "conq5": "【战区争夺】#5",
    "conq6": "【战区争夺】#6",
    "dbttla": "【战区攻防】#1",
    "dbttlb": "【战区攻防】#2",
    "cnva": "【护航行动】#1",
    "cnvb": "【护航行动】#2",
    "aadefa": "【防空/空袭】#1",
    "aadefb": "【防空/空袭】#2",
    "bto": "【战区突破】",
    "dbldom": "【绝对控制】",
    "deepdefa": "【纵深防御/突破】#1",
    "deepdefb": "【纵深防御/突破】#2",
    "ctf": "【夺旗模式】",
    "tdm": "【团队死斗】",
    "ffa": "【死亡竞赛】",
    "acsd": "【海岸防御】",
}

BULLET_TYPE_FALLBACKS = {
    "apt_ball": "穿甲曳光弹·AP-T",
    "cannon_ball": "实心炮弹",
    "heat_cluster_bomb": "破甲子母弹",
    "snowball_tank": "雪球",
}

EXCLUDED_UNIT_SUBSTRINGS = ("_football", "aaa_dummy", "tutorial_target")


def clean_wt(s: str) -> str:
    s = s.replace("\u200b", "").replace("\u200c", "").replace("\u200d", "").replace("\ufeff", "").replace("\xa0", " ")
    cyr_to_lat = str.maketrans({"А": "A", "В": "B", "Е": "E", "К": "K", "М": "M", "Н": "H", "О": "O", "Р": "P", "С": "C", "Т": "T", "Х": "X"})
    s = s.translate(cyr_to_lat)
    return re.sub(r"\s+", " ", s).strip()


def strip_wt_glyphs(s: str) -> tuple[str, list[str]]:
    badges = []
    out_chars = []
    for ch in s:
        if ch in SUBTREE_GLYPHS:
            badges.append(SUBTREE_GLYPHS[ch])
        elif 0xE000 <= ord(ch) <= 0xF8FF or 0x2400 <= ord(ch) <= 0x243F or (0x2500 <= ord(ch) <= 0x27BF):
            continue
        else:
            out_chars.append(ch)
    return re.sub(r"\s+", " ", "".join(out_chars)).strip(), badges


def load_csv(fn: str) -> dict[str, tuple[str, str]]:
    out = {}
    with open(LANG_DIR / fn, encoding="utf-8-sig", newline="") as f:
        r = csv.reader(f, delimiter=";", quotechar='"')
        hdr = next(r)
        ei, ci = hdr.index("<English>"), hdr.index("<Chinese>")
        for row in r:
            if len(row) > max(ei, ci) and row[0] and not row[0].startswith("<"):
                out[row[0].lower()] = (clean_wt(row[ei]), clean_wt(row[ci]))
    return out


def first(v):
    return v[0] if isinstance(v, list) and v else v


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


def num(v, default=0.0) -> float:
    v = first(v)
    return float(v) if isinstance(v, (int, float)) and math.isfinite(v) else default


def opt_pos(v):
    v = first(v)
    return float(v) if isinstance(v, (int, float)) and math.isfinite(v) and v > 0 else None


def parse_rocket_shell(rb: dict, outer_b: dict | None, ammo_key: str) -> dict | None:
    b = outer_b or {}
    cal = opt_pos(rb.get("caliber")) or opt_pos(b.get("ballisticCaliber")) or opt_pos(b.get("caliber"))
    m = opt_pos(rb.get("mass")) or opt_pos(b.get("mass"))
    if not cal or not m:
        return None

    sid = str(first(rb.get("bulletName")) or first(b.get("bulletName")) or ammo_key)
    btype = str(first(rb.get("bulletType")) or first(b.get("bulletType")) or "rocket_tank")

    force0 = num(rb.get("force") if rb.get("force") is not None else rb.get("force0"), 0.0)
    t_fire0 = num(rb.get("timeFire") if rb.get("timeFire") is not None else rb.get("timeFire0"), 0.0)
    m_end0 = opt_pos(rb.get("massEnd")) or m

    force1 = num(rb.get("force1"), 0.0)
    t_fire1 = num(rb.get("timeFire1"), 0.0)
    m_end1 = opt_pos(rb.get("massEnd1")) or m_end0
    t_delay = num(rb.get("timeFireDelay"), 0.0)

    v_start_raw = rb.get("startSpeed") if rb.get("startSpeed") is not None else b.get("speed")
    v_start = num(v_start_raw, 0.0)
    v_end = num(rb.get("endSpeed"), 0.0)
    v_max_stat = num(rb.get("maxSpeed"), 0.0)

    guided = bool(rb.get("operated") or rb.get("guidanceType") or rb.get("autoAiming"))
    cx_k = opt_pos(rb.get("CxK"))
    cx = cx_k or opt_pos(rb.get("dragCx")) or opt_pos(b.get("Cx"))
    max_dist = num(rb.get("maxDistance") or rb.get("rangeMax") or b.get("maxDistance"), 0.0)
    t_life = num(rb.get("timeLife") or b.get("timeLife"), 0.0)
    disp_deg = num(rb.get("maxDeltaAngle") or b.get("maxDeltaAngle"), 0.0)

    if force0 > 0.0 and t_fire0 > 0.0:
        dv0 = force0 / (0.5 * (m + m_end0)) * t_fire0
        dv1 = (force1 / (0.5 * (m_end0 + m_end1)) * t_fire1) if (force1 > 0.0 and t_fire1 > 0.0) else 0.0
        v_imp = v_start + dv0 + dv1
        if 0.0 < v_max_stat < 1800.0:
            v_disp = v_max_stat
        elif 0.0 < v_end < 1800.0:
            v_disp = v_end
        else:
            v_disp = v_imp
        if v_disp <= 0.0:
            return None
        return {
            "ammo": ammo_key,
            "id": sid,
            "type": btype,
            "v0": round(float(v_disp), 3),
            "m": float(m),
            "cal": float(cal),
            "cx": round(float(cx), 5) if cx is not None else None,
            "maxDist": float(max_dist),
            "rocket": {
                "vStart": round(float(v_start), 3),
                "force0": round(float(force0), 3),
                "tFire0": round(float(t_fire0), 4),
                "mEnd0": round(float(m_end0), 4),
                "force1": round(float(force1), 3),
                "tFire1": round(float(t_fire1), 4),
                "mEnd1": round(float(m_end1), 4),
                "tDelay": round(float(t_delay), 4),
                "tLife": round(float(t_life), 3),
                "maxDist": round(float(max_dist), 1),
                "disp": round(float(disp_deg), 3),
                "cxMach": bool(cx_k is not None),
                "guided": guided,
            },
        }
    else:
        # Separated booster dart (e.g. Pantsir 95Ya6 / Starstreak) or pure unpowered rocket entry
        v_launch = v_max_stat if 0.0 < v_max_stat < 1800.0 else (v_end if 0.0 < v_end < 1800.0 else v_start)
        if v_launch <= 0.0:
            return None
        dart_m = opt_pos(rb.get("warheadMass")) or m
        return {
            "ammo": ammo_key,
            "id": sid,
            "type": btype,
            "v0": round(float(v_launch), 3),
            "m": float(dart_m),
            "cal": float(cal),
            "cx": round(float(cx), 5) if cx is not None else None,
            "maxDist": float(max_dist),
            "rocket": {
                "vStart": round(float(v_launch), 3),
                "force0": 0.0,
                "tFire0": 0.0,
                "mEnd0": float(dart_m),
                "force1": 0.0,
                "tFire1": 0.0,
                "mEnd1": float(dart_m),
                "tDelay": 0.0,
                "tLife": round(float(t_life), 3),
                "maxDist": round(float(max_dist), 1),
                "disp": round(float(disp_deg), 3),
                "cxMach": bool(cx_k is not None),
                "guided": guided,
            } if guided else None,
        }


def parse_conventional_shell(b: dict, ammo_key: str) -> dict | None:
    if not isinstance(b, dict):
        return None
    if isinstance(b.get("rocket"), dict):
        return parse_rocket_shell(b["rocket"], b, ammo_key)
    v0 = opt_pos(b.get("speed"))
    m = opt_pos(b.get("mass"))
    cal = opt_pos(b.get("ballisticCaliber")) or opt_pos(b.get("caliber"))
    if not v0 or not m or not cal:
        return None
    sid = str(first(b.get("bulletName")) or ammo_key)
    btype = str(first(b.get("bulletType")) or "?")
    cx = opt_pos(b.get("Cx"))
    max_dist = num(b.get("maxDistance"), 0.0)
    return {
        "ammo": ammo_key,
        "id": sid,
        "type": btype,
        "v0": round(float(v0), 3),
        "m": float(m),
        "cal": float(cal),
        "cx": round(float(cx), 5) if cx is not None else None,
        "maxDist": float(max_dist),
    }


def extract_weapon_shells(w: dict, wlow: str = "") -> tuple[list[dict], bool]:
    """Return (shells, is_primary_or_heavy_weapon)."""
    out, seen = [], set()
    has_rocket_gun = bool(w.get("rocketGun"))

    def add_item(s: dict | None):
        if not s:
            return
        if s.get("rocket") is None:
            s.pop("rocket", None)
        rk = s.get("rocket") or {}
        dedup_key = (s["id"].lower(), round(s["v0"], 1), round(s["cx"] or -1.0, 4), round(rk.get("force0", 0.0), 0))
        if dedup_key not in seen:
            seen.add(dedup_key)
            out.append(s)

    # 1. Top-level bullet(s) & rocket(s)
    for b in as_list(w.get("bullet")):
        if isinstance(b, dict):
            add_item(parse_conventional_shell(b, "default"))
    for r in as_list(w.get("rocket")):
        if isinstance(r, dict):
            has_rocket_gun = True
            add_item(parse_rocket_shell(r, None, "default"))

    # 2. Named ammo blocks inside weapon dict
    for key, val in w.items():
        if key in ("bullet", "rocket") or not isinstance(val, dict):
            continue
        for b in as_list(val.get("bullet")):
            if isinstance(b, dict):
                add_item(parse_conventional_shell(b, key))
        for r in as_list(val.get("rocket")):
            if isinstance(r, dict):
                has_rocket_gun = True
                add_item(parse_rocket_shell(r, None, key))

    max_cal = max((s["cal"] for s in out), default=0.0)
    has_rocket_round = any("rocket" in s or "rocket" in s["type"] or "atgm" in s["type"] or "sam" in s["type"] for s in out)
    is_mg = ("machinegun" in wlow) and max_cal < 0.020 and not has_rocket_round and not has_rocket_gun
    is_heavy = (not is_mg) and (bool(w.get("cannon")) or has_rocket_gun or has_rocket_round or max_cal >= 0.020)
    return out, is_heavy


def estimate_cx(tanks: list[dict]):
    by_type, by_family, everything = {}, {}, []
    shells = [s for t in tanks for g in t["guns"] for s in g["shells"]]
    for s in shells:
        if s["cx"] is not None:
            by_type.setdefault(s["type"], []).append(s["cx"])
            by_family.setdefault(s["type"].split("_")[0], []).append(s["cx"])
            everything.append(s["cx"])
    for s in shells:
        if s["cx"] is None:
            pool = by_type.get(s["type"]) or by_family.get(s["type"].split("_")[0]) or everything or [0.35]
            s["cx"] = round(float(np.median(pool)), 4)
            s["cxEst"] = True


def translate_mission(mn: str, map_cn: str, versus: dict[str, tuple[str, str]]) -> str:
    low = mn.lower()
    direct = versus.get(f"missions/{low}") or versus.get(low)
    if direct and direct[1]:
        cn, _ = strip_wt_glyphs(direct[1])
        return f"{cn} ({mn})"
    if low.startswith("aaa_"):
        return f"【地空对垒】{map_cn} ({mn})"
    if "_ny_ctf" in low:
        return f"【圣诞夺旗】{map_cn} ({mn})"
    parts = low.split("_")
    suf = parts[-1]
    mode_cn = MISSION_MODE_LABELS.get(suf, f"【{suf.upper()}】")
    variant = f" ({parts[-2]}号布局)" if len(parts) >= 2 and parts[-2].isdigit() else ""
    return f"{mode_cn}{variant} · {mn}"


def translate_gun(weapon: str, weap: dict[str, tuple[str, str]]) -> str:
    w = weapon.lower()
    base_no_suf = re.sub(r"_(user_cannon|user_machinegun|rocket_launcher)$", "", w)
    for cand in (
        f"weapons/{w}/short",
        f"weapons/{w}",
        w,
        f"weapons/{base_no_suf}/short",
        f"weapons/{base_no_suf}",
    ):
        hit = weap.get(cand)
        if hit and hit[1]:
            cn, _ = strip_wt_glyphs(hit[1])
            if cn:
                return cn
    if "oerlikon" in w:
        return "20 毫米 厄利孔机炮"
    if "snowball" in w:
        return "75 毫米 雪球发射器"
    return base_no_suf.replace("_", " ")


def translate_shell_name(sid: str, weap: dict[str, tuple[str, str]]) -> tuple[str, str]:
    """Return (cn_name, en_name)."""
    low = sid.lower()
    for cand in (low, f"{low}/name/short", f"{low}/name", f"weapons/{low}/short", f"weapons/{low}"):
        hit = weap.get(cand)
        if hit and (hit[1] or hit[0]):
            cn, _ = strip_wt_glyphs(hit[1] or hit[0])
            en, _ = strip_wt_glyphs(hit[0] or sid)
            if cn:
                return cn, en
    mg_map = {
        "default": "默认弹链",
        "universal": "通用弹链",
        "ap": "穿甲弹",
        "api": "穿甲燃烧弹",
        "apit": "穿甲燃烧曳光弹",
    }
    if "_" in low:
        parts = sid.split("_")
        last = parts[-1].lower()
        if last in mg_map:
            cal_gun = " ".join(parts[:-1]).replace("7 62mm", "7.62mm").replace("7 92mm", "7.92mm")
            return f"{cal_gun} {mg_map[last]}", sid
    if low == "default":
        return "默认弹药", "default"
    return sid, sid


def translate_bullet_type(bt: str, weap: dict[str, tuple[str, str]]) -> str:
    low = bt.lower()
    if low in BULLET_TYPE_FALLBACKS:
        return BULLET_TYPE_FALLBACKS[low]
    full_hit = weap.get(f"{low}/name")
    short_hit = weap.get(f"{low}/name/short")
    full_cn = strip_wt_glyphs(full_hit[1])[0] if full_hit and full_hit[1] else ""
    short_cn = strip_wt_glyphs(short_hit[1])[0] if short_hit and short_hit[1] else ""
    short_en = strip_wt_glyphs(short_hit[0])[0] if short_hit and short_hit[0] else bt.removesuffix("_tank").upper()
    if short_en == "R":
        short_en = "Rocket"
    if full_cn and short_en:
        if full_cn.upper() == short_en.upper() or short_en.upper() in full_cn.upper():
            return full_cn
        return f"{full_cn}·{short_en}"
    return full_cn or short_cn or short_en


def extract_all_tanks_from_datamine(units: dict[str, tuple[str, str]], weap_lang: dict[str, tuple[str, str]]) -> list[dict]:
    weapon_files = {p.stem.lower(): p for p in WEAP_DIR.glob("*.blkx")}
    preset_files = {p.stem.lower(): p for p in (TANK_DIR / "weaponpresets").glob("*.blkx")} if (TANK_DIR / "weaponpresets").exists() else {}
    weapon_cache: dict[str, tuple[list[dict], bool] | None] = {}

    raw_tanks = []
    for f in sorted(TANK_DIR.glob("*.blkx")):
        tid = f.stem
        low = tid.lower()
        if any(ex in low for ex in EXCLUDED_UNIT_SUBSTRINGS):
            continue
        shop_hit = units.get(f"{low}_shop") or units.get(f"{low}_0") or units.get(low)
        if not shop_hit or not (shop_hit[0] or shop_hit[1]):
            continue

        t_json = json.loads(f.read_text("utf-8"))
        sources = [t_json]
        for sub in iter_dicts(t_json):
            bp = sub.get("blk")
            if isinstance(bp, str) and "weaponpresets" in bp.lower():
                pstem = Path(bp.replace("\\", "/")).stem.lower()
                pf = preset_files.get(pstem)
                if pf:
                    sources.append(json.loads(pf.read_text("utf-8")))

        # Collect weapon mounts, merging pitch limits if multiple barrels share the exact same weapon .blk
        mounts: dict[str, dict] = {}
        mount_order: list[str] = []
        for src in sources:
            for sub in iter_dicts(src):
                bp = sub.get("blk")
                if not isinstance(bp, str) or "groundmodels_weapons" not in bp.lower():
                    continue
                if str(sub.get("triggerGroup", "")).lower() == "smoke":
                    continue
                wstem = Path(bp.replace("\\", "/")).stem
                wlow = wstem.lower()
                if "dummy" in wlow or "smoke_grenade" in wlow or "smoke_mortar" in wlow:
                    continue
                wf = weapon_files.get(wlow)
                if not wf:
                    continue
                if wlow not in weapon_cache:
                    wd = json.loads(wf.read_text("utf-8"))
                    shells, is_heavy = extract_weapon_shells(wd, wlow)
                    weapon_cache[wlow] = (shells, is_heavy) if shells else None
                cached = weapon_cache[wlow]
                if not cached:
                    continue

                raw_pitch = (sub.get("limits") or {}).get("pitch")
                if isinstance(raw_pitch, list) and len(raw_pitch) >= 2 and all(isinstance(x, (int, float)) for x in raw_pitch[:2]):
                    p0, p1 = float(raw_pitch[0]), float(raw_pitch[1])
                else:
                    p0, p1 = -10.0, 20.0

                if wlow not in mounts:
                    mounts[wlow] = {
                        "weapon": wstem,
                        "pitch": [p0, p1],
                        "shells": [json.loads(json.dumps(s)) for s in cached[0]],
                        "is_heavy": cached[1],
                    }
                    mount_order.append(wlow)
                else:
                    mounts[wlow]["pitch"][0] = min(mounts[wlow]["pitch"][0], p0)
                    mounts[wlow]["pitch"][1] = max(mounts[wlow]["pitch"][1], p1)

        heavy_guns = [mounts[k] for k in mount_order if mounts[k]["is_heavy"]]
        mg_guns = [mounts[k] for k in mount_order if not mounts[k]["is_heavy"]]
        chosen = heavy_guns if heavy_guns else mg_guns
        if not chosen:
            continue

        for g in chosen:
            g.pop("is_heavy", None)
            g["title"] = translate_gun(g["weapon"], weap_lang)
            for s in g["shells"]:
                cn_s, en_s = translate_shell_name(s["id"], weap_lang)
                s["name"] = cn_s
                s["en"] = en_s
                s["typeTitle"] = translate_bullet_type(s["type"], weap_lang)

        raw_tanks.append((tid, shop_hit, chosen))

    # Format display names & deduplicate identical names within the same nation
    prelim = []
    name_counts = {}
    for tid, shop_hit, guns in raw_tanks:
        low = tid.lower()
        nation_code = low.split("_", 1)[0]
        ntag = NATION_TAGS.get(nation_code, nation_code.upper())
        en_shop = shop_hit[0]
        cn_raw = WIKI_DISPLAY_NICKNAMES.get(low) or shop_hit[1] or en_shop
        cn_clean, badges = strip_wt_glyphs(cn_raw)

        full_hit = units.get(f"{low}_0")
        class_hit = units.get(f"{low}_2")
        extra_search_parts = [
            strip_wt_glyphs(en_shop)[0],
            strip_wt_glyphs(full_hit[0])[0] if full_hit else "",
            strip_wt_glyphs(full_hit[1])[0] if full_hit else "",
            strip_wt_glyphs(class_hit[0])[0] if class_hit else "",
            strip_wt_glyphs(class_hit[1])[0] if class_hit else "",
            WIKI_EXTRA_SEARCH.get(low, ""),
        ]
        extra_search = " ".join(x for x in extra_search_parts if x)

        base_display = f"[{ntag}] {cn_clean}"
        prelim.append((tid, en_shop, extra_search, ntag, cn_clean, badges, base_display, guns))
        name_counts[base_display] = name_counts.get(base_display, 0) + 1

    tanks = []
    used_displays = set()
    for tid, en_shop, extra_search, ntag, cn_clean, badges, base_display, guns in prelim:
        low = tid.lower()
        is_special_suffix = any(low.endswith(s) or s in low for s in ("_sm", "_event", "_yt_cup", "_race", "_killstreak", "_trb", "_twitch"))
        if name_counts[base_display] > 1:
            if badges:
                cand = f"[{ntag}·{badges[0]}] {cn_clean}"
            elif low.endswith("_sm"):
                cand = f"[{ntag}·礼包] {cn_clean}"
            elif is_special_suffix:
                cand = f"[{ntag}·活动] {cn_clean}"
            else:
                cand = base_display
            if cand in used_displays:
                cand = f"{cand} ({tid.split('_', 1)[-1]})"
            display = cand
        else:
            if badges and badges[0] in ("东德", "匈牙利", "瑞士", "荷兰", "挪威", "希腊", "印尼", "哥伦比亚"):
                display = f"[{ntag}·{badges[0]}] {cn_clean}"
            else:
                display = base_display
        used_displays.add(display)

        tanks.append({
            "id": tid,
            "kind": "ground",
            "name": display,
            "en": extra_search,
            "model": LITE_A_MODEL,
            "guns": guns,
        })

    estimate_cx(tanks)
    return tanks


HELI_EXTRA_NICKNAMES = {
    "ka_50": "卡50 ka50 黑鲨 black shark 直升机",
    "ka_52": "卡52 ka52 短吻鳄 alligator 直升机",
    "ka_29": "卡29 ka29 直升机",
    "mi_4av": "米4 mi4 小母牛 直升机",
    "mi_8": "米8 mi8 河马 hip 直升机",
    "mi_24": "米24 mi24 雌鹿 hind 直升机",
    "mi_28": "米28 mi28 浩劫 havoc 直升机",
    "mi_35": "米35 mi35 雌鹿 直升机",
    "z_9": "直9 z9 海豚 直升机",
    "z_10": "直10 z10 霹雳火 武直10 直升机",
    "z_19": "直19 z19 黑旋风 武直19 直升机",
    "ah_1": "ah1 眼镜蛇 cobra 超级眼镜蛇 蝰蛇 直升机",
    "uh_1": "uh1 休伊 huey 直升机",
    "ah_64": "ah64 阿帕奇 apache 长弓 直升机",
    "oh_58": "oh58 奇奥瓦 kiowa 直升机",
    "ah_6": "ah6 小鸟 little bird 直升机",
    "rah_66": "rah66 科曼奇 comanche 直升机",
    "ah_60": "ah60 黑鹰 战鹰 black hawk 直升机",
    "tiger": "虎式 虎UHT 虎HAD 虎HAP tiger 直升机",
    "bo_105": "bo105 直升机",
    "lynx": "山猫 lynx 直升机",
    "rooivalk": "茶隼 rooivalk 直升机",
    "a_129": "a129 芒古斯塔 猫鼬 mangusta 直升机",
}


def resolve_air_weapon_blk(rel_blk: str, cache: dict[str, dict | None]) -> tuple[dict | None, str]:
    """Unwrap nested containers/*.blk up to 4 levels until reaching the actual weapon/rocketGun .blkx."""
    cur = rel_blk.lstrip("/").replace("\\", "/")
    for _ in range(5):
        if not cur.endswith("x"):
            cur_x = cur + "x"
        else:
            cur_x = cur
        if cur_x not in cache:
            p = DM_ACES / cur_x
            cache[cur_x] = json.loads(p.read_text("utf-8")) if p.exists() else None
        jd = cache[cur_x]
        if not jd:
            return None, Path(cur).stem
        if jd.get("container") and isinstance(jd.get("blk"), str):
            cur = jd["blk"].lstrip("/").replace("\\", "/")
            continue
        return jd, Path(cur).stem
    return None, Path(cur).stem


def extract_all_helicopters_from_datamine(units: dict[str, tuple[str, str]], weap_lang: dict[str, tuple[str, str]]) -> list[dict]:
    wpcost_path = DM_CHAR / "config" / "wpcost.blkx"
    wpcost = json.loads(wpcost_path.read_text("utf-8")) if wpcost_path.exists() else {}
    air_cache: dict[str, dict | None] = {}

    raw_helis = []
    for f in sorted(FM_DIR.glob("*.blkx")):
        stem = f.stem
        low = stem.lower()
        if any(ex in low for ex in ("_killstreak", "_tutorial", "_dummy", "_event0", "_ai")):
            continue
        if wpcost and stem not in wpcost and low not in wpcost:
            continue
        shop_hit = units.get(f"{low}_shop") or units.get(f"{low}_0") or units.get(low)
        if not shop_hit or not (shop_hit[0] or shop_hit[1]):
            continue

        d = json.loads(f.read_text("utf-8"))
        if "helicopter" not in d and not d.get("isHelicopter"):
            continue

        ccip_rk = bool(d.get("haveCCIPForRocket"))
        sources = [d]
        for pr in as_list((d.get("weapon_presets") or {}).get("preset")):
            if isinstance(pr, dict) and isinstance(pr.get("blk"), str):
                bp = pr["blk"].lstrip("/").replace("\\", "/")
                if "empty" in bp.lower():
                    continue
                pf = DM_ACES / (bp + ("" if bp.endswith("x") else "x"))
                if pf.exists():
                    sources.append(json.loads(pf.read_text("utf-8")))

        rocket_shells: list[dict] = []
        seen_rk = set()
        sec_mounts: dict[str, dict] = {}
        sec_order: list[str] = []

        for src in sources:
            for sub in iter_dicts(src):
                trig = str(sub.get("trigger", "")).lower()
                bp = sub.get("blk")
                if not isinstance(bp, str) or not trig or trig in ("countermeasures", "flares", "bombs", "torpedoes", "aam"):
                    continue
                if "dummy" in bp.lower():
                    continue
                wjd, wstem = resolve_air_weapon_blk(bp, air_cache)
                if not wjd:
                    continue
                wlow = wstem.lower()
                shells, is_heavy = extract_weapon_shells(wjd, wlow)
                if not shells:
                    continue

                if trig == "rockets":
                    for s in shells:
                        s_copy = json.loads(json.dumps(s))
                        # Ensure unguided rockets are never flagged guided
                        if s_copy.get("rocket"):
                            s_copy["rocket"]["guided"] = False
                        cn_s, en_s = translate_shell_name(s_copy["id"], weap_lang)
                        if cn_s == s_copy["id"]:
                            cn_s2, en_s2 = translate_shell_name(wstem, weap_lang)
                            if cn_s2 != wstem:
                                cn_s, en_s = cn_s2, en_s2
                        s_copy["name"] = cn_s
                        s_copy["en"] = f"{en_s} {wstem}"
                        s_copy["typeTitle"] = "非制导火箭弹" if s_copy["type"] == "rocket_tank" else translate_bullet_type(s_copy["type"], weap_lang)
                        rk = s_copy.get("rocket") or {}
                        dk = (s_copy["name"].lower(), round(s_copy["v0"], 1), round(rk.get("force0", 0.0), 0), round(s_copy.get("maxDist", 0.0), 0))
                        if dk not in seen_rk:
                            seen_rk.add(dk)
                            rocket_shells.append(s_copy)
                elif trig in ("cannon", "additional gun", "turret", "gunner0", "atgm"):
                    if trig != "atgm" and not is_heavy:
                        continue
                    if wlow not in sec_mounts:
                        s_list = []
                        for s in shells:
                            sc = json.loads(json.dumps(s))
                            cn_s, en_s = translate_shell_name(sc["id"], weap_lang)
                            if cn_s == sc["id"]:
                                cn_s, en_s = translate_shell_name(wstem, weap_lang)
                            sc["name"] = cn_s
                            sc["en"] = en_s
                            sc["typeTitle"] = translate_bullet_type(sc["type"], weap_lang)
                            s_list.append(sc)
                        sec_mounts[wlow] = {
                            "weapon": wstem,
                            "title": "空地导弹" if trig == "atgm" else translate_gun(wstem, weap_lang),
                            "pitch": [-45.0, 45.0],
                            "shells": s_list,
                        }
                        sec_order.append(wlow)

        guns = []
        if rocket_shells:
            # Put higher-caliber / medium-to-heavy iconic rockets (like S-8KO, S-13OF, 90-1, FS70, Hydra-70) first
            rocket_shells.sort(key=lambda x: (0 if 0.068 <= x["cal"] <= 0.14 else 1, -x["cal"]))
            guns.append({
                "weapon": "heli_unguided_rockets",
                "title": "非制导火箭弹",
                "pitch": [-45.0, 45.0],
                "shells": rocket_shells,
            })
        for k in sec_order:
            guns.append(sec_mounts[k])
        if not guns:
            continue

        wp_entry = wpcost.get(stem) or wpcost.get(low) or {}
        country = str(wp_entry.get("country", "")).lower().removeprefix("country_")
        ntag = NATION_TAGS.get(country, country.upper() or "直")
        raw_helis.append((stem, ntag, shop_hit, ccip_rk, guns))

    prelim = []
    name_counts = {}
    for stem, ntag, shop_hit, ccip_rk, guns in raw_helis:
        low = stem.lower()
        en_shop = shop_hit[0]
        cn_raw = shop_hit[1] or en_shop
        cn_clean, badges = strip_wt_glyphs(cn_raw)
        full_hit = units.get(f"{low}_0")
        nick_extra = " ".join(v for k, v in HELI_EXTRA_NICKNAMES.items() if low.startswith(k) or k in low)
        extra_parts = [
            "直升机 武直 helicopter heli",
            strip_wt_glyphs(en_shop)[0],
            strip_wt_glyphs(full_hit[0])[0] if full_hit else "",
            strip_wt_glyphs(full_hit[1])[0] if full_hit else "",
            nick_extra,
        ]
        extra_search = " ".join(x for x in extra_parts if x)
        base_display = f"[{ntag}·武直] {cn_clean}"
        prelim.append((stem, en_shop, extra_search, ntag, cn_clean, badges, base_display, ccip_rk, guns))
        name_counts[base_display] = name_counts.get(base_display, 0) + 1

    helis = []
    used = set()
    for stem, en_shop, extra_search, ntag, cn_clean, badges, base_display, ccip_rk, guns in prelim:
        if name_counts[base_display] > 1 and badges:
            cand = f"[{ntag}·{badges[0]}·武直] {cn_clean}"
        elif name_counts[base_display] > 1:
            cand = f"{base_display} ({stem})"
        else:
            cand = base_display
        if cand in used:
            cand = f"{cand} ({stem})"
        used.add(cand)
        helis.append({
            "id": f"heli_{stem}",
            "kind": "heli",
            "ccip": ccip_rk,
            "name": cand,
            "en": extra_search,
            "model": LITE_A_MODEL,
            "guns": guns,
        })

    estimate_cx(helis)
    return helis


def collect_helipads_recursive(p: Path, visited: set[Path]) -> list[tuple[str, str, list]]:
    if p in visited or not p.exists():
        return []
    visited.add(p)
    d = json.loads(p.read_text("utf-8"))
    out = []
    ogs = (d.get("units") or {}).get("objectGroups") or []
    for g in as_list(ogs):
        if not isinstance(g, dict):
            continue
        uc = str(g.get("unit_class", ""))
        nm = str(g.get("name", ""))
        tm = g.get("tm")
        if ("helipad" in uc.lower() or "helipad" in nm.lower()) and isinstance(tm, list) and len(tm) == 4:
            out.append((nm, uc, tm))
    for ir in as_list((d.get("imports") or {}).get("import_record")):
        if isinstance(ir, dict) and isinstance(ir.get("file"), str):
            f_rel = ir["file"].lstrip("/").replace("\\", "/")
            sp = DM_MIS / (f_rel + ("" if f_rel.endswith("x") else "x"))
            if sp.exists():
                out.extend(collect_helipads_recursive(sp, visited))
    return out


def default_pad_extents(uc: str) -> tuple[float, float]:
    u = uc.lower()
    if "beton_b" in u:
        return (260.0, 136.0)
    if "mud_b" in u:
        return (166.0, 142.0)
    if "arctic" in u:
        return (158.0, 136.0)
    return (140.0, 138.0)


def extract_map_helipads(
    lv: str,
    mjson: dict,
    cta_dirs: dict[str, Path],
    prefab_meta: dict | None = None,
    true_y_for_map: dict | None = None,
) -> list[dict]:
    base = lv.removeprefix("avg_")
    mis_names = [ms["name"] for ms in mjson.get("missions", [])]
    cands = [cta_dirs.get(base), cta_dirs.get(lv)] + [
        d for k, d in cta_dirs.items() if base in k or any(mn.startswith(k) for mn in mis_names)
    ]
    found: dict[str, tuple[str, list]] = {}
    for cd in dict.fromkeys(x for x in cands if x):
        for bf in sorted(cd.rglob("*.blkx")):
            if bf.stem in mis_names or "heli_spawn" in bf.stem.lower() or "air_spawn" in bf.stem.lower():
                for nm, uc, tm in collect_helipads_recursive(bf, set()):
                    if nm not in found:
                        found[nm] = (uc, tm)

    # Compute tank battlefield center to orient takeoff direction toward the battle area
    bounds = mjson.get("bounds") or [
        mjson["ox"],
        mjson["oz"],
        mjson["ox"] + mjson["width"] * mjson["cell"],
        mjson["oz"] + mjson["height"] * mjson["cell"],
    ]
    cx = 0.5 * (bounds[0] + bounds[2])
    cz = 0.5 * (bounds[1] + bounds[3])

    order_spec = [
        ("t1_helipad", "t1_near", "1队近距停机坪 (H1)", "1队近H1", 1, False),
        ("t1_helipad_farspawn", "t1_far", "1队远距停机坪 (H2)", "1队远H2", 1, True),
        ("t2_helipad", "t2_near", "2队近距停机坪 (H1)", "2队近H1", 2, False),
        ("t2_helipad_farspawn", "t2_far", "2队远距停机坪 (H2)", "2队远H2", 2, True),
    ]
    pads = []
    for raw_nm, pid, label, short, team, is_far in order_spec:
        if raw_nm not in found:
            continue
        uc_raw, tm = found[raw_nm]
        uc_low = uc_raw.lower()
        px, py, pz = float(tm[3][0]), float(tm[3][1]), float(tm[3][2])
        if true_y_for_map and pid in true_y_for_map:
            py = float(true_y_for_map[pid])
        ux_raw, uz_raw = float(tm[0][0]), float(tm[0][2])
        ulen = math.hypot(ux_raw, uz_raw) or 1.0
        ux, uz = ux_raw / ulen, uz_raw / ulen
        vx, vz = -ux, -uz
        if vx * (cx - px) + vz * (cz - pz) < 0:
            vx, vz = -vx, -vz
        yaw_deg = (math.degrees(math.atan2(vx, vz)) + 360.0) % 360.0
        if prefab_meta and uc_low in prefab_meta:
            hx = float(prefab_meta[uc_low]["hx"])
            hz = float(prefab_meta[uc_low]["hz"])
        else:
            hx, hz = default_pad_extents(uc_low)
        pads.append({
            "id": pid,
            "raw": raw_nm,
            "uc": uc_low,
            "name": label,
            "short": short,
            "team": team,
            "far": is_far,
            "x": round(px, 2),
            "y": round(py, 2),
            "z": round(pz, 2),
            "yaw": round(yaw_deg, 1),
            "ux": round(ux, 5),
            "uz": round(uz, 5),
            "hx": round(hx, 1),
            "hz": round(hz, 1),
        })
    return pads


def main():
    locs = load_csv("missions_locations.csv")
    versus = load_csv("missions_versus.csv")
    units = load_csv("units.csv")
    weap = load_csv("units_weaponry.csv")

    with open(LITE_EXE, "rb") as f:
        f.seek(-24, 2)
        off, length = struct.unpack("<QQ", f.read(16))
        magic = f.read(8)
        assert magic == b"WTVPACK2"
        f.seek(off)
        pack_idx = json.loads(f.read(length).decode("utf-8"))
        entries = {p: (o, n, c) for p, o, n, c in pack_idx}

        def read_entry(rel: str) -> bytes:
            o, n, c = entries[rel]
            f.seek(o)
            return lzma.decompress(f.read(n))

        idx_raw = read_entry("index.js").decode("utf-8").strip()
        index = json.loads(idx_raw[len("WTV_INDEX("):].rstrip(";").rstrip()[:-1])

        # 1. Translate Maps
        map_titles_cn = {}
        for m in index["maps"]:
            lv = m["name"]
            if lv in WINTER_MAP_OVERRIDES:
                en, cn = WINTER_MAP_OVERRIDES[lv]
            else:
                hit = locs.get(f"location/{lv.lower()}") or locs.get(f"location/{lv.lower().removeprefix('avg_')}")
                en, cn = hit if hit else (lv, lv)
                cn = cn.replace(" - 陆战", "")
            cn, _ = strip_wt_glyphs(cn)
            m["title"] = cn
            m["en"] = en
            map_titles_cn[lv] = cn

        # 2. Extract ALL Ground Vehicles + Armed Helicopters & Ammunition from War-Thunder-Datamine
        ground_vehicles = extract_all_tanks_from_datamine(units, weap)
        helicopters = extract_all_helicopters_from_datamine(units, weap)
        index["tanks"] = ground_vehicles + helicopters

        # 3. Translate Missions & Extract Helipad Coordinates inside every maps/<level>.js
        cta_root = DM_MIS / "gamedata" / "missions" / "cta" / "tanks"
        cta_dirs = {d.name.lower(): d for d in cta_root.iterdir() if d.is_dir()} if cta_root.exists() else {}
        air_idx_path = ROOT / "i18n_src" / "airspace_raw" / "air_meta_index.json"
        air_meta_map = json.loads(air_idx_path.read_text(encoding="utf-8")) if air_idx_path.exists() else {}
        true_y_path = ROOT / "i18n_src" / "airspace_raw" / "helipad_true_y.json"
        true_y_all = json.loads(true_y_path.read_text(encoding="utf-8")) if true_y_path.exists() else {}
        prefab_path = ROOT / "i18n_src" / "airspace_raw" / "helipad_prefabs" / "prefabs.json"
        prefab_meta = json.loads(prefab_path.read_text(encoding="utf-8")) if prefab_path.exists() else {}
        updated_entries: dict[str, bytes] = {}
        maps_with_pads = 0
        for m in index["maps"]:
            lv = m["name"]
            rel = f"maps/{lv}.js"
            mraw = read_entry(rel).decode("utf-8").strip()
            mjson = json.loads(mraw[len("WTV_MAP("):].rstrip(";").rstrip()[:-1])
            for mis in mjson.get("missions", []):
                mis["title"] = translate_mission(mis["name"], map_titles_cn[lv], versus)
            pads = extract_map_helipads(lv, mjson, cta_dirs, prefab_meta, true_y_all.get(lv))
            mjson["helipads"] = pads
            if lv in air_meta_map:
                mjson["air"] = air_meta_map[lv]
            m["helipads"] = len(pads)
            if pads:
                maps_with_pads += 1
            new_mtxt = "WTV_MAP(" + json.dumps(mjson, ensure_ascii=False, separators=(",", ":")) + ");\n"
            updated_entries[rel] = new_mtxt.encode("utf-8")

        new_idx_txt = "WTV_INDEX(" + json.dumps(index, ensure_ascii=False, separators=(",", ":")) + ");\n"
        updated_entries["index.js"] = new_idx_txt.encode("utf-8")

    cache_dir = ROOT / "i18n_src" / "patched_js"
    (cache_dir / "maps").mkdir(parents=True, exist_ok=True)
    for rel, data_bytes in updated_entries.items():
        out_p = cache_dir / rel
        out_p.parent.mkdir(parents=True, exist_ok=True)
        out_p.write_bytes(data_bytes)

    total_guns = sum(len(t["guns"]) for t in index["tanks"])
    total_shells = sum(len(s) for t in index["tanks"] for g in t["guns"] for s in [g["shells"]])
    rocket_shells = sum(1 for t in index["tanks"] for g in t["guns"] for s in g["shells"] if "rocket" in s)
    print(f"Imported {len(ground_vehicles)} ground vehicles + {len(helicopters)} helicopters = {len(index['tanks'])} total vehicles!")
    print(f"Total weapons: {total_guns}, shells/rockets/missiles: {total_shells} ({rocket_shells} rocket/missile), helipads on {maps_with_pads}/{len(index['maps'])} maps!")

    # Verify key vehicles & helicopters
    by_id = {t["id"].lower(): t for t in index["tanks"]}
    for check_id in [
        "ussr_bm_31_12", "ussr_bm_13n", "germ_sturmmorser_sturmtiger",
        "heli_ka_52", "heli_mi_24v", "heli_z_10", "heli_z_19", "heli_ah_64d", "heli_ah_1g", "heli_uh_1b",
    ]:
        t = by_id.get(check_id)
        if t:
            gsum = [(g["title"], g["pitch"], [(s["name"], s["typeTitle"], s["v0"], s.get("rocket")) for s in g["shells"][:3]]) for g in t["guns"][:2]]
            print(f"  CHECK {check_id}: {t['name']} (ccip={t.get('ccip')}) -> {gsum}")
        else:
            print(f"  [MISSING!] {check_id}")


if __name__ == "__main__":
    main()
