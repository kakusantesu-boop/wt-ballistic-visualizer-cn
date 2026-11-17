//! Extracts the outer helicopter airspace terrain (`LandMesh` ray-tracer + `HM2` + clean helipad grading)
//! and bakes its `1024x1024` `GroundBaker` satellite colour map for every map in `patched_js/maps/*.js`.
//! Also computes and saves each helipad's true ground-snapped elevation (`helipad_true_y.json`),
//! eliminating stale Dagor editor `tm[3][1]` height errors across all 56 maps.
//!
//! Usage: `cargo run --release --bin airspace -- "<War Thunder dir>" "<patched_js/maps dir>" "<out raw dir>"`

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use wt_unpack::landclass::{decode_colormaps, GroundBaker, GroundMaterial};
use wt_unpack::level::{Heightmap, Level, TriMesh};
use wt_unpack::rendinst::{ResourceIndex, LAND_CLASS};
use wt_unpack::texlib::TextureLibrary;
use wt_unpack::vehicle;

const GRID_N: usize = 1024;
const PAD_MARGIN: f64 = 2400.0;

fn default_ux() -> f64 {
    1.0
}
fn default_half() -> f64 {
    138.0
}

#[derive(Deserialize)]
struct Helipad {
    id: String,
    x: f64,
    #[allow(dead_code)]
    y: f64,
    z: f64,
    #[serde(default)]
    uc: String,
    #[serde(default = "default_ux")]
    ux: f64,
    #[serde(default)]
    uz: f64,
    #[serde(default = "default_half")]
    hx: f64,
    #[serde(default = "default_half")]
    hz: f64,
}

#[derive(Deserialize)]
struct MapJs {
    name: String,
    width: usize,
    height: usize,
    cell: f64,
    ox: f64,
    oz: f64,
    #[serde(default)]
    water: Option<f64>,
    #[serde(default)]
    helipads: Vec<Helipad>,
}

fn parse_map_js(path: &Path) -> Result<MapJs> {
    let s = fs::read_to_string(path)?;
    let s = s.trim();
    let body = s
        .strip_prefix("WTV_MAP(")
        .and_then(|r| r.trim_end_matches(';').trim_end().strip_suffix(')'))
        .with_context(|| format!("invalid WTV_MAP wrapper in {}", path.display()))?;
    Ok(serde_json::from_str(body)?)
}

fn sample_hm2(hm: &Heightmap, wx: f32, wz: f32) -> Option<f32> {
    let fx = (wx - hm.origin[0]) / hm.cell_size;
    let fz = (wz - hm.origin[1]) / hm.cell_size;
    let (w, h) = (hm.width as usize, hm.height as usize);
    if fx < 0.0 || fz < 0.0 || fx > (w - 1) as f32 || fz > (h - 1) as f32 {
        return None;
    }
    let x0 = (fx.floor() as usize).min(w.saturating_sub(2));
    let z0 = (fz.floor() as usize).min(h.saturating_sub(2));
    let (ax, az) = (fx - x0 as f32, fz - z0 as f32);
    let at = |dx: usize, dz: usize| hm.metres(hm.data[(z0 + dz).min(h - 1) * w + (x0 + dx).min(w - 1)]);
    let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
    Some((a * (1.0 - ax) + b * ax) * (1.0 - az) + (c * (1.0 - ax) + d * ax) * az)
}

fn sample_grid_bilinear(grid: &[f32], ox: f32, oz: f32, cell: f32, w: usize, h: usize, wx: f32, wz: f32) -> f32 {
    let fx = ((wx - ox) / cell).clamp(0.0, (w - 1) as f32);
    let fz = ((wz - oz) / cell).clamp(0.0, (h - 1) as f32);
    let x0 = (fx.floor() as usize).min(w.saturating_sub(2));
    let z0 = (fz.floor() as usize).min(h.saturating_sub(2));
    let (ax, az) = (fx - x0 as f32, fz - z0 as f32);
    let at = |dx: usize, dz: usize| grid[(z0 + dz).min(h - 1) * w + (x0 + dx).min(w - 1)];
    let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
    (a * (1.0 - ax) + b * ax) * (1.0 - az) + (c * (1.0 - ax) + d * ax) * az
}

fn rasterize_window(mesh: &TriMesh, ox: f32, oz: f32, cell: f32, w: usize, h: usize) -> Vec<f32> {
    let mut grid = vec![f32::NAN; w * h];
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[tri[k] as usize]);
        let g = |p: [f32; 3]| [(p[0] - ox) / cell, (p[2] - oz) / cell, p[1]];
        let (a, b, c) = (g(a), g(b), g(c));
        let min_x = a[0].min(b[0]).min(c[0]);
        let max_x = a[0].max(b[0]).max(c[0]);
        let min_y = a[1].min(b[1]).min(c[1]);
        let max_y = a[1].max(b[1]).max(c[1]);
        if max_x < 0.0 || max_y < 0.0 || min_x > (w - 1) as f32 || min_y > (h - 1) as f32 {
            continue;
        }
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
        if area.abs() < 1e-10 {
            continue;
        }
        let x0 = min_x.ceil().max(0.0) as usize;
        let x1 = (max_x.floor().max(0.0) as usize).min(w - 1);
        let y0 = min_y.ceil().max(0.0) as usize;
        let y1 = (max_y.floor().max(0.0) as usize).min(h - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32, y as f32);
                let wb = ((px - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (py - a[1])) / area;
                let wc = ((b[0] - a[0]) * (py - a[1]) - (px - a[0]) * (b[1] - a[1])) / area;
                let wa = 1.0 - wb - wc;
                if wa < -1e-4 || wb < -1e-4 || wc < -1e-4 {
                    continue;
                }
                let hv = wa * a[2] + wb * b[2] + wc * c[2];
                let slot = &mut grid[y * w + x];
                if slot.is_nan() || hv > *slot {
                    *slot = hv;
                }
            }
        }
    }
    grid
}

fn fill_nan_holes(grid: &mut [f32], w: usize, h: usize, fallback: f32) {
    if !grid.iter().any(|v| v.is_nan()) {
        return;
    }
    for _ in 0..1024 {
        let mut changed = false;
        let mut any_nan = false;
        let prev = grid.to_vec();
        for y in 0..h {
            for x in 0..w {
                let idx = y * w + x;
                if !prev[idx].is_nan() {
                    continue;
                }
                any_nan = true;
                let mut sum = 0.0f32;
                let mut cnt = 0u32;
                if x > 0 && !prev[idx - 1].is_nan() { sum += prev[idx - 1]; cnt += 1; }
                if x + 1 < w && !prev[idx + 1].is_nan() { sum += prev[idx + 1]; cnt += 1; }
                if y > 0 && !prev[idx - w].is_nan() { sum += prev[idx - w]; cnt += 1; }
                if y + 1 < h && !prev[idx + w].is_nan() { sum += prev[idx + w]; cnt += 1; }
                if cnt > 0 {
                    grid[idx] = sum / cnt as f32;
                    changed = true;
                }
            }
        }
        if !any_nan || !changed {
            break;
        }
    }
    for v in grid.iter_mut() {
        if v.is_nan() {
            *v = fallback;
        }
    }
}

fn fallback_rgb(grid: &[f32], w: usize, h: usize, lo: f32, hi: f32) -> Vec<u8> {
    let span = (hi - lo).max(1.0);
    let mut rgb = vec![0u8; w * h * 3];
    for j in 0..h {
        for i in 0..w {
            let c = grid[j * w + i];
            let tt = ((c - lo) / span).clamp(0.0, 1.0);
            let (r, g, b) = if c < 0.0 {
                (52.0, 82.0, 108.0)
            } else if tt < 0.5 {
                (76.0 + 100.0 * tt, 102.0 + 65.0 * tt, 64.0 + 45.0 * tt)
            } else {
                (126.0 + 95.0 * (tt - 0.5), 134.0 + 85.0 * (tt - 0.5), 86.0 + 105.0 * (tt - 0.5))
            };
            let o = (j * w + i) * 3;
            rgb[o] = r.clamp(0.0, 255.0) as u8;
            rgb[o + 1] = g.clamp(0.0, 255.0) as u8;
            rgb[o + 2] = b.clamp(0.0, 255.0) as u8;
        }
    }
    rgb
}

fn export_one(
    game: &Path,
    index: &ResourceIndex,
    textures: &TextureLibrary,
    mjs: &MapJs,
    out_dir: &Path,
) -> Result<BTreeMap<String, f32>> {
    let bin_path = game.join("levels").join(format!("{}.bin", mjs.name));
    let raw = vehicle::map(&bin_path)?;
    let lv = Level::parse(&raw).with_context(|| format!("parsing {}", mjs.name))?;

    let (mut min_x, mut min_z) = (mjs.ox, mjs.oz);
    let (mut max_x, mut max_z) = (mjs.ox + (mjs.width - 1) as f64 * mjs.cell, mjs.oz + (mjs.height - 1) as f64 * mjs.cell);
    for p in &mjs.helipads {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_z = min_z.min(p.z);
        max_z = max_z.max(p.z);
    }
    min_x -= PAD_MARGIN;
    max_x += PAD_MARGIN;
    min_z -= PAD_MARGIN;
    max_z += PAD_MARGIN;

    if let Some(lm) = lv.landmesh()? {
        let ext = lm.extent();
        min_x = min_x.max(ext[0] as f64 + 64.0);
        min_z = min_z.max(ext[1] as f64 + 64.0);
        max_x = max_x.min(ext[2] as f64 - 64.0);
        max_z = max_z.min(ext[3] as f64 - 64.0);
    }

    let raw_span = (max_x - min_x).max(max_z - min_z).max(8192.0);
    let cell = (raw_span / (GRID_N - 1) as f64).ceil().max(8.0) as f32;
    let span = cell as f64 * (GRID_N - 1) as f64;
    let cx = 0.5 * (min_x + max_x);
    let cz = 0.5 * (min_z + max_z);
    let ax0 = ((cx - 0.5 * span) / cell as f64).round() as f32 * cell;
    let az0 = ((cz - 0.5 * span) / cell as f64).round() as f32 * cell;

    let hm = lv.heightmap()?;
    let mut grid = if let Some(lm) = lv.landmesh()? {
        if let Some(mesh) = lm.tracer_mesh()? {
            rasterize_window(&mesh, ax0, az0, cell, GRID_N, GRID_N)
        } else {
            vec![f32::NAN; GRID_N * GRID_N]
        }
    } else {
        vec![f32::NAN; GRID_N * GRID_N]
    };

    // Stamp exact HM2 heights inside the inner tank map so the boundary has zero vertical step
    if let Some(h) = &hm {
        for j in 0..GRID_N {
            let wz = az0 + j as f32 * cell;
            for i in 0..GRID_N {
                let wx = ax0 + i as f32 * cell;
                if let Some(hv) = sample_hm2(h, wx, wz) {
                    grid[j * GRID_N + i] = hv;
                }
            }
        }
    }

    let fallback_h = hm.as_ref().map_or(0.0, |h| h.height_min);
    fill_nan_holes(&mut grid, GRID_N, GRID_N, fallback_h);

    // Snapshot natural untouched ground before grading so overlapping/nearby samples always read pure LandMesh/HM2
    let natural_grid = grid.clone();
    let mut pad_true_y = BTreeMap::new();

    for p in &mjs.helipads {
        let (px, pz) = (p.x as f32, p.z as f32);
        let ul = (p.ux.hypot(p.uz)).max(1e-6);
        let (ux, uz) = ((p.ux / ul) as f32, (p.uz / ul) as f32);
        let (hx, hz) = (p.hx.max(125.0) as f32, p.hz.max(125.0) as f32);

        let in_hm2 = hm.as_ref().and_then(|h| sample_hm2(h, px, pz)).is_some();
        let y_true = if in_hm2 {
            // Inside HM2 (e.g. avg_syria near pads): HM2 is untouched, so place the 3D pad deck just above the highest HM2 point under the apron
            let h_ref = hm.as_ref().unwrap();
            let mut hi_hm = sample_hm2(h_ref, px, pz).unwrap_or(0.0);
            let (sx_n, sz_n) = (16i32, 16i32);
            for ix in -sx_n..=sx_n {
                let lx = (ix as f32 / sx_n as f32) * (hx - 6.0);
                for iz in -sz_n..=sz_n {
                    let lz = (iz as f32 / sz_n as f32) * (hz - 6.0);
                    let wx = px + lx * ux - lz * uz;
                    let wz = pz + lx * uz + lz * ux;
                    if let Some(hv) = sample_hm2(h_ref, wx, wz) {
                        hi_hm = hi_hm.max(hv);
                    }
                }
            }
            hi_hm + 0.06
        } else {
            // Sample natural LandMesh ground over the helipad's spawn apron and take median elevation
            let mut samples = Vec::with_capacity(289);
            for ix in -8..=8 {
                let lx = (ix as f32 / 8.0) * (hx * 0.78);
                for iz in -8..=8 {
                    let lz = (iz as f32 / 8.0) * (hz * 0.78);
                    let wx = px + lx * ux - lz * uz;
                    let wz = pz + lx * uz + lz * ux;
                    samples.push(sample_grid_bilinear(&natural_grid, ax0, az0, cell, GRID_N, GRID_N, wx, wz));
                }
            }
            samples.sort_by(|a, b| a.total_cmp(b));
            let mut med = samples[samples.len() / 2];
            if let Some(wl) = mjs.water {
                med = med.max(wl as f32 + 1.6);
            }
            med
        };

        pad_true_y.insert(p.id.clone(), (y_true * 100.0).round() / 100.0);

        if !in_hm2 {
            // Grade the outer LandMesh heightmap smoothly under the rotated rectangular pad (0.25m below 3D pad deck to prevent Z-fight)
            let y_pad_ground = y_true - 0.25;
            let hx_flat = hx + cell * 0.85;
            let hz_flat = hz + cell * 0.85;
            let shoulder = 85.0f32;
            let bound_r = hx_flat.hypot(hz_flat) + shoulder + cell;
            let i0 = (((px - bound_r - ax0) / cell).floor().max(0.0) as usize).min(GRID_N - 1);
            let i1 = (((px + bound_r - ax0) / cell).ceil().max(0.0) as usize).min(GRID_N - 1);
            let j0 = (((pz - bound_r - az0) / cell).floor().max(0.0) as usize).min(GRID_N - 1);
            let j1 = (((pz + bound_r - az0) / cell).ceil().max(0.0) as usize).min(GRID_N - 1);
            for j in j0..=j1 {
                let wz = az0 + j as f32 * cell;
                for i in i0..=i1 {
                    let wx = ax0 + i as f32 * cell;
                    if let Some(h) = &hm {
                        if sample_hm2(h, wx, wz).is_some() {
                            continue;
                        }
                    }
                    let (dx, dz) = (wx - px, wz - pz);
                    let lx = dx * ux + dz * uz;
                    let lz = -dx * uz + dz * ux;
                    let d_rect = (lx.abs() - hx_flat).max(0.0).hypot((lz.abs() - hz_flat).max(0.0));
                    let idx = j * GRID_N + i;
                    if d_rect <= 0.0 {
                        grid[idx] = y_pad_ground;
                    } else if d_rect < shoulder {
                        let t = d_rect / shoulder;
                        let w = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
                        let cur = grid[idx];
                        grid[idx] = cur + (y_pad_ground - cur) * w;
                    }
                }
            }
        }
    }

    let (mut hmin, mut hmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in &grid {
        hmin = hmin.min(v);
        hmax = hmax.max(v);
    }
    if (hmax - hmin) < 1e-3 {
        hmax = hmin + 1.0;
    }

    // Quantize heights to u16
    let scale = 65535.0 / (hmax - hmin);
    let mut u16_bytes = Vec::with_capacity(GRID_N * GRID_N * 2);
    for &v in &grid {
        let q = ((v - hmin) * scale).round().clamp(0.0, 65535.0) as u16;
        u16_bytes.extend_from_slice(&q.to_le_bytes());
    }

    // Bake 1024x1024 RGB ground colours over the exact same grid extent
    let mut rgb_bytes = if let Some(gm) = GroundMaterial::load(&lv, index)? {
        if let Some(weights) = gm.weights {
            let corner = weights.origin;
            let maps = decode_colormaps(&gm.classes, textures, 512, &mut |_| {});
            let baker = GroundBaker::new(weights, &gm.classes, maps, corner);
            let rect = [
                ax0 - cell * 0.5,
                az0 - cell * 0.5,
                ax0 + (GRID_N as f32 - 0.5) * cell,
                az0 + (GRID_N as f32 - 0.5) * cell,
            ];
            let img = baker.bake(rect, cell);
            let mut rgb = Vec::with_capacity(GRID_N * GRID_N * 3);
            for p in img.rgba.chunks_exact(4).take(GRID_N * GRID_N) {
                rgb.extend_from_slice(&p[..3]);
            }
            rgb
        } else {
            fallback_rgb(&grid, GRID_N, GRID_N, hmin, hmax)
        }
    } else {
        fallback_rgb(&grid, GRID_N, GRID_N, hmin, hmax)
    };

    // Tint the airfield clearing on the 2D outer satellite map so zoomed-out 2D view shows crisp rectangular pads
    for p in &mjs.helipads {
        let (px, pz) = (p.x as f32, p.z as f32);
        let ul = (p.ux.hypot(p.uz)).max(1e-6);
        let (ux, uz) = ((p.ux / ul) as f32, (p.uz / ul) as f32);
        let (hx, hz) = (p.hx.max(125.0) as f32, p.hz.max(125.0) as f32);
        let is_snow = p.uc.contains("arctic") || p.uc.contains("snow");
        let pad_tint: [f32; 3] = if is_snow { [205.0, 214.0, 224.0] } else { [138.0, 136.0, 128.0] };
        let rwy_tint: [f32; 3] = if is_snow { [120.0, 126.0, 136.0] } else { [92.0, 96.0, 102.0] };

        let bound_r = hx.hypot(hz) + 24.0;
        let i0 = (((px - bound_r - ax0) / cell).floor().max(0.0) as usize).min(GRID_N - 1);
        let i1 = (((px + bound_r - ax0) / cell).ceil().max(0.0) as usize).min(GRID_N - 1);
        let j0 = (((pz - bound_r - az0) / cell).floor().max(0.0) as usize).min(GRID_N - 1);
        let j1 = (((pz + bound_r - az0) / cell).ceil().max(0.0) as usize).min(GRID_N - 1);
        for j in j0..=j1 {
            let wz = az0 + j as f32 * cell;
            for i in i0..=i1 {
                let wx = ax0 + i as f32 * cell;
                let (dx, dz) = (wx - px, wz - pz);
                let lx = dx * ux + dz * uz;
                let lz = -dx * uz + dz * ux;
                if lx.abs() <= hx + 12.0 && lz.abs() <= hz + 12.0 {
                    let edge = ((hx + 12.0 - lx.abs()).min(hz + 12.0 - lz.abs()) / 18.0).clamp(0.0, 1.0);
                    let in_rwy = lx.abs() <= (hx - 14.0) && lz.abs() <= 16.0;
                    let (tgt, alpha) = if in_rwy { (rwy_tint, 0.72 * edge) } else { (pad_tint, 0.46 * edge) };
                    let o = (j * GRID_N + i) * 3;
                    for c in 0..3 {
                        rgb_bytes[o + c] = (rgb_bytes[o + c] as f32 * (1.0 - alpha) + tgt[c] * alpha).clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }

    // Binary file format:
    // magic "WTVA" (4B), w u32, h u32, ox f32, oz f32, cell f32, hmin f32, hmax f32 (total 32B header)
    // followed by u16_le[w*h] + rgb[w*h*3] (row 0 = smallest Z)
    let mut out = Vec::with_capacity(32 + u16_bytes.len() + rgb_bytes.len());
    out.extend_from_slice(b"WTVA");
    out.extend_from_slice(&(GRID_N as u32).to_le_bytes());
    out.extend_from_slice(&(GRID_N as u32).to_le_bytes());
    out.extend_from_slice(&ax0.to_le_bytes());
    out.extend_from_slice(&az0.to_le_bytes());
    out.extend_from_slice(&cell.to_le_bytes());
    out.extend_from_slice(&hmin.to_le_bytes());
    out.extend_from_slice(&hmax.to_le_bytes());
    out.extend_from_slice(&u16_bytes);
    out.extend_from_slice(&rgb_bytes);

    fs::write(out_dir.join(format!("{}.air.raw", mjs.name)), out)?;
    let ys: Vec<String> = pad_true_y.iter().map(|(k, v)| format!("{k}:{v:.1}m")).collect();
    println!(
        "  {} -> {}x{} @ {}m, span={:.1}km, h=[{:.1}..{:.1}m], pads=[{}]",
        mjs.name,
        GRID_N,
        GRID_N,
        cell,
        (GRID_N as f32 * cell) / 1000.0,
        hmin,
        hmax,
        ys.join(", ")
    );
    Ok(pad_true_y)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        anyhow::bail!("usage: airspace <wt game dir> <patched_js/maps dir> <out raw dir>");
    }
    let game = PathBuf::from(&args[1]);
    let maps_dir = PathBuf::from(&args[2]);
    let out_dir = PathBuf::from(&args[3]);
    fs::create_dir_all(&out_dir)?;

    println!("Indexing War Thunder landclasses & textures from {} ...", game.display());
    let index = ResourceIndex::build(&game, &[LAND_CLASS])?;
    let textures = TextureLibrary::open(&game)?;
    println!("Indexed {} landclass resources. Exporting 56 outer airspace maps...", index.len());

    let mut entries: Vec<PathBuf> = fs::read_dir(&maps_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "js") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("avg_")))
        .collect();
    entries.sort();

    let mut all_true_y: BTreeMap<String, BTreeMap<String, f32>> = BTreeMap::new();
    let mut ok = 0usize;
    for (idx, p) in entries.iter().enumerate() {
        let mjs = parse_map_js(p)?;
        print!("[{:2}/{}] ", idx + 1, entries.len());
        let py = export_one(&game, &index, &textures, &mjs, &out_dir)
            .with_context(|| format!("exporting {}", mjs.name))?;
        all_true_y.insert(mjs.name, py);
        ok += 1;
    }
    fs::write(out_dir.join("helipad_true_y.json"), serde_json::to_string_pretty(&all_true_y)?)?;
    println!("Successfully exported {ok}/{} outer airspace maps + helipad_true_y.json!", entries.len());
    Ok(())
}
