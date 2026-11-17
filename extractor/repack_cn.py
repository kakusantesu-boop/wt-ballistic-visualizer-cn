#!/usr/bin/env python3
"""Fast lossless repackager & distributable ZIP builder for 婴儿弹道计算机."""

import json
import lzma
import struct
import time
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKSPACE = ROOT.parent
PLAIN_EXE = ROOT / "rust" / "target" / "release" / "wtv.exe"
ORIG_LITE = ROOT / "dist" / "WT-Ballistic-Visualizer-Lite.exe"
PATCHED_DIR = ROOT / "i18n_src" / "patched_js"

RELEASE_DIR = ROOT / "dist" / "婴儿弹道计算机"
OUT_EXE = RELEASE_DIR / "婴儿弹道计算机.exe"
OUT_README = RELEASE_DIR / "使用说明.txt"
OUT_ZIP = WORKSPACE / "婴儿弹道计算机.zip"
MAGIC = b"WTVPACK2"

USER_GUIDE = """====================================================================
                  婴儿弹道计算机 · 使用说明
====================================================================

【1. 直接运行（免安装单文件版）】
- 请先将压缩包完整解压到任意文件夹（不要直接在压缩包预览窗口内双击运行）。
- 双击运行「婴儿弹道计算机.exe」（适用于 64 位 Windows 10 / 11，免装运行库、无需开启游戏、完全离线）。
- 若首次打开弹出蓝色提示「Windows 已保护你的电脑」（因个人编译软件未购买商业数字证书）：
  点击窗口上的「更多信息」 -> 再点击下方出现的「仍要运行」即可正常启动。

【2. 快速上手（陆战火炮 & 火箭炮）】
1) 在左侧栏选择「作战地图」、「载具型号」和「选用弹药」（上方可切换「全部 / 陆战 / 直升机」分类）。
   下拉框支持直接输入中文、英文、外号或型号缩写搜索（例如：废弃工厂、安德柳沙、bm31、喀秋莎、突击虎、99a、2s38、卡-52、直-10、AH-64）。
2) 在地图上点「鼠标左键」放置己方开火点 A，点「鼠标右键」放置目标点 B。
3) 左侧「弹道解算 A → B」面板会实时给出：
   - 水平距离、斜距、海拔高差、方位角，以及视线是否被山体/房屋/树冠遮挡；
   - 低伸弹道（直射）与高抛弹道（曲射）两组解：能否命中、火炮仰角（角度与密位）、高于目标 B 的抬高量、飞行时间与落点存速。
4) 在「观察视角」中可切换：
   - 2D 俯视地图（底部同步显示 A → B 沿线地形与弹道高低剖面）
   - 3D 自由环绕（左键拖动旋转，右键或 Shift+拖动平移，滚轮缩放，双击地面居中）
   - 3D 炮口/机头视角（从 A 点望向 B 点）
   - 3D 瞄准镜视角（带密位横标尺与当前弹药距离分划，鼠标滚轮调节 1×~24× 倍率）

【3. 武装直升机火箭弹模式（新增）】
1) 点击载具分类「直升机」（共收录 114 架武装直升机），选择机型与非制导火箭弹（如 S-8KO、九头蛇 70、S-13OF、90-1 型等）。
2) 地图自动解锁外圈 20 km 空域，标出全部 56 张地图的 4 处真实停机坪（1队近H1 / 1队远H2 / 2队近H1 / 2队远H2）及起飞朝向。
3) 点击「1队近H1」等按钮可一键落位停机坪，或在空域任意位置左键放置悬停机位 A；也可点击「标定B: 战区 A/B/C」快速设靶。
4) 可自由调节「悬停离地高 AGL (m)」与「平飞空速 (km/h)」（0 为悬停，前飞时自动计入机身初速修正仰角）。
5) 射表面板直接以游戏内武直 HUD 同款「罗盘方位角 (°) + 机头俯仰角 (°)」为核心显示，并同步生成：
   - 本局全部占领战区（A/B/C）与双方陆战出生点的多目标速查射表；
   - 距离梯度速查射表、±0.5° 仰角齐射落点跨度，以及火箭弹定时/定距半空自爆（如 S-8KO 15 秒自毁）极限射程圈与超距诊断。

【4. 常用快捷键与进阶工具】
- W / A / S / D（或方向键）：先点击一次地图区域后，可直接移动战车或飞行直升机（直升机模式下按 Q / E 升降悬停高度）
- R 键：开启 / 关闭战场边界显示
- / 键：快速聚焦地图搜索框
- 寻找最高点：一键标出当前任务作战区内（不含红区）的最高地形点
- 全空域总览（直升机模式）：一键缩放至包含四座外圈停机坪与中央陆战区的全景视野
- A 点火力覆盖 / 可打到 B 的阵地 / A 点视野通视分析：多线程全图扫描图层
- 选择禁区红区：在「禁区红区」开关下方点击展开，可按编号单独开关某块出生点红区

【5. 已知物理边界说明】
- 本工具完全离线运行，不读取或接触《战争雷霆》游戏进程，无任何反作弊风险。
- 弹道采用质点气动物理、马赫数阻力曲线与两级固体火箭发动机推力/减重/自毁引信模型解算，不含游戏内随机风偏。
- 中心 4×4 km 陆战核心区含 1m 级 HM2 高程、房屋巨石与树冠三维遮挡；外圈 15~25 km 停机坪空域含战雷原版 LandMesh 全域地形高程与卫星地表贴图（外圈远景不含独立房屋三维网格）。
- 标有「估算风阻」的老式炮弹因游戏原始文件未公开风阻系数，采用同弹种中位数估算，远距离落点存在近似偏差。
- 标有「制导」的导弹（ATGM / SAM）在游戏内由瞄准线或雷达制导修正航迹，主要用于查验通视视线（LOS）、极速与射程范围。

【5. 致谢与声明】
- 本软件基于开源项目 Xander009/wt-ballistic-visualizer 本地化与扩展构建，载具与弹药词库源自战雷拆包开源库（War-Thunder-Datamine）与中文 Wiki。
- 非官方玩家离线工具，与 Gaijin Entertainment 无关联。
"""


def check_no_pack(exe: Path):
    with open(exe, "rb") as f:
        f.seek(0, 2)
        sz = f.tell()
        if sz >= 24:
            f.seek(sz - 8)
            if f.read(8) == MAGIC:
                raise RuntimeError(f"{exe} already has WTVPACK2 appended!")


def safe_replace(tmp_file: Path, target_file: Path):
    if target_file.exists():
        try:
            target_file.unlink()
        except PermissionError:
            old_file = target_file.with_suffix(f".{int(time.time())}.old.exe")
            target_file.rename(old_file)
    tmp_file.rename(target_file)


def main():
    t0 = time.time()
    assert PLAIN_EXE.exists(), f"Missing {PLAIN_EXE}; run cargo build --release first"
    assert ORIG_LITE.exists(), f"Missing {ORIG_LITE}"
    assert (PATCHED_DIR / "index.js").exists(), f"Missing {PATCHED_DIR / 'index.js'}; run localize_data.py first"
    check_no_pack(PLAIN_EXE)

    RELEASE_DIR.mkdir(parents=True, exist_ok=True)

    with open(ORIG_LITE, "rb") as src_f:
        src_f.seek(-24, 2)
        off, length = struct.unpack("<QQ", src_f.read(16))
        assert src_f.read(8) == MAGIC, "Invalid WTVPACK2 magic in source bundle"
        src_f.seek(off)
        orig_index: list[list] = json.loads(src_f.read(length).decode("utf-8"))

        tmp_out = OUT_EXE.with_suffix(".partial")
        new_index = []
        replaced = 0
        seen_rels = set()
        xz_cache_dir = ROOT / "i18n_src" / "airspace_xz_cache"
        xz_cache_dir.mkdir(parents=True, exist_ok=True)

        with open(tmp_out, "wb") as out_f:
            exe_bytes = PLAIN_EXE.read_bytes()
            out_f.write(exe_bytes)
            pos = len(exe_bytes)

            for rel, o_off, o_len, codec in orig_index:
                seen_rels.add(rel)
                patched_file = PATCHED_DIR / rel
                if patched_file.exists():
                    raw = patched_file.read_bytes()
                    z = lzma.compress(raw, format=lzma.FORMAT_XZ, preset=6)
                    replaced += 1
                else:
                    src_f.seek(o_off)
                    z = src_f.read(o_len)
                out_f.write(z)
                new_index.append([rel, pos, len(z), codec])
                pos += len(z)

            extra_top = PATCHED_DIR / "helipads.js"
            if extra_top.exists() and "helipads.js" not in seen_rels:
                z = lzma.compress(extra_top.read_bytes(), format=lzma.FORMAT_XZ, preset=6)
                out_f.write(z)
                new_index.append(["helipads.js", pos, len(z), 1])
                pos += len(z)
                seen_rels.add("helipads.js")

            extra_files = sorted((PATCHED_DIR / "maps").glob("*.air.*"))
            added_air = 0
            for ef in extra_files:
                rel = f"maps/{ef.name}"
                if rel in seen_rels:
                    continue
                cache_xz = xz_cache_dir / f"{ef.name}.xz"
                if cache_xz.exists() and cache_xz.stat().st_mtime >= ef.stat().st_mtime:
                    z = cache_xz.read_bytes()
                else:
                    z = lzma.compress(ef.read_bytes(), format=lzma.FORMAT_XZ, preset=6)
                    cache_xz.write_bytes(z)
                out_f.write(z)
                new_index.append([rel, pos, len(z), 1])
                pos += len(z)
                added_air += 1

            idx_bytes = json.dumps(new_index, separators=(",", ":")).encode("utf-8")
            out_f.write(idx_bytes)
            out_f.write(struct.pack("<QQ", pos, len(idx_bytes)))
            out_f.write(MAGIC)

    safe_replace(tmp_out, OUT_EXE)
    OUT_README.write_text(USER_GUIDE.replace("\n", "\r\n"), encoding="utf-8-sig")

    sz_mb = OUT_EXE.stat().st_size / (1024 * 1024)
    print(f"Wrote {OUT_EXE} ({len(new_index)} entries, {replaced} localized JS, {added_air} outer airspace assets, {sz_mb:.1f} MB) in {time.time() - t0:.1f}s")

    t_zip = time.time()
    tmp_zip = OUT_ZIP.with_suffix(".partial.zip")
    with zipfile.ZipFile(tmp_zip, "w", allowZip64=True) as zf:
        zf.write(OUT_README, arcname="婴儿弹道计算机/使用说明.txt", compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        zf.write(OUT_EXE, arcname="婴儿弹道计算机/婴儿弹道计算机.exe", compress_type=zipfile.ZIP_STORED)
    if OUT_ZIP.exists():
        OUT_ZIP.unlink()
    tmp_zip.rename(OUT_ZIP)

    # Verify zip archive header & CRC integrity
    with zipfile.ZipFile(OUT_ZIP, "r") as zf:
        bad = zf.testzip()
        assert bad is None, f"Corrupted entry in zip: {bad}"
        names = zf.namelist()

    zip_mb = OUT_ZIP.stat().st_size / (1024 * 1024)
    print(f"Packaged & verified {OUT_ZIP} ({zip_mb:.1f} MB, entries={names}) in {time.time() - t_zip:.1f}s")


if __name__ == "__main__":
    main()
