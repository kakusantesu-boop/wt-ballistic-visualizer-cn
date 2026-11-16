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
TANK_DIR = DM_ACES / "gamedata" / "units" / "tankmodels"
WEAP_DIR = DM_ACES / "gamedata" / "weapons" / "groundmodels_weapons"
LITE_EXE = ROOT / "dist" / "WT-Ballistic-Visualizer-Lite.exe"

LITE_A_MODEL = "leopard_2a4"

NATION_TAGS = {
    "us": "美",
    "germ": "德",
    "ussr": "苏",
    "uk": "英",
    "jp": "日",
    "cn": "中",
    "it": "意",
    "fr": "法",
    "sw": "瑞",
    "il": "以",
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

    guided = bool(rb.get("operated") or rb.get("guidanceType") or rb.get("autoAiming"))
    cx = opt_pos(rb.get("CxK")) or opt_pos(rb.get("dragCx")) or opt_pos(b.get("Cx"))
    max_dist = num(rb.get("maxDistance") or rb.get("rangeMax") or b.get("maxDistance"), 0.0)

    if force0 > 0.0 and t_fire0 > 0.0:
        if v_end > 0.0:
            v_disp = v_end
        else:
            dv0 = force0 / (0.5 * (m + m_end0)) * t_fire0
            dv1 = (force1 / (0.5 * (m_end0 + m_end1)) * t_fire1) if (force1 > 0.0 and t_fire1 > 0.0) else 0.0
            v_disp = v_start + dv0 + dv1
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
                "guided": guided,
            },
        }
    else:
        # Separated booster dart (e.g. Pantsir 95Ya6 / Starstreak) or pure unpowered rocket entry
        v_launch = v_end if v_end > 0.0 else v_start
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
            "name": display,
            "en": extra_search,
            "model": LITE_A_MODEL,
            "guns": guns,
        })

    estimate_cx(tanks)
    return tanks


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

        # 2. Extract ALL Ground Vehicles & Ammunition from GitHub War-Thunder-Datamine
        index["tanks"] = extract_all_tanks_from_datamine(units, weap)

        # 3. Translate Missions inside every maps/<level>.js
        updated_entries: dict[str, bytes] = {}
        for m in index["maps"]:
            lv = m["name"]
            rel = f"maps/{lv}.js"
            mraw = read_entry(rel).decode("utf-8").strip()
            mjson = json.loads(mraw[len("WTV_MAP("):].rstrip(";").rstrip()[:-1])
            for mis in mjson.get("missions", []):
                mis["title"] = translate_mission(mis["name"], map_titles_cn[lv], versus)
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
    print(f"Imported {len(index['tanks'])} ground vehicles, {total_guns} weapons, {total_shells} shells/rockets/missiles ({rocket_shells} with rocket/missile parameters)!")

    # Verify key vehicles requested by user
    by_id = {t["id"].lower(): t for t in index["tanks"]}
    for check_id in ["ussr_bm_31_12", "ussr_bm_13n", "ussr_bm_8_24", "germ_sturmmorser_sturmtiger", "jp_type_75_mlrs", "us_m4_sherman_calliope", "ussr_rbt_5", "uk_churchill_avre", "cn_hj_9", "cn_plz_05"]:
        t = by_id.get(check_id)
        if t:
            gsum = [(g["title"], g["pitch"], [(s["name"], s["typeTitle"], s["v0"], s.get("rocket")) for s in g["shells"]]) for g in t["guns"]]
            print(f"  CHECK {check_id}: {t['name']} -> {gsum}")
        else:
            print(f"  [MISSING!] {check_id}")


if __name__ == "__main__":
    main()
