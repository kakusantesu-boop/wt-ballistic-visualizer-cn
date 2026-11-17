//! Extracts the 6 War Thunder helicopter airfield prefabs (`dynhelipad_*.blk` / `dynaf_arctic_d.blk`)
//! into one shared WTVO v3 model library (`models.wtvo`) + per-prefab metadata & instance transforms (`prefabs.json`).
//!
//! Each prefab gets:
//!   1. A crisp procedural 1m-precision 3D Airfield Pad Deck (`pad_deck_<kind>`) with:
//!      - Level compacted gravel/earth/snow terrace + perimeter concrete curb
//!      - Sloped 3D earth/rock/snow embankment skirt (-24 m) so hillside bases have zero floating gaps
//!      - Central takeoff runway strip (with centerline dashes, threshold bars, and takeoff arrow along -X)
//!      - Asphalt taxiway connectors + central command [H] pad + all 16 `spawn:p3` concrete [H] helipads
//!   2. Every real War Thunder 3D RendInst building/tower/hangar/barrack/tent/fuel-tank/container placed at its
//!      exact `tm:m` from `gamedata/objectgroups/<prefab>.blk`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use wt_formats::grp::GrpPack;
use wt_unpack::blk::{Block, Value};
use wt_unpack::collision::{Collision, CollisionNode, NodeKind, COLLISION};
use wt_unpack::rendinst::{is_facing_leaves, model_descs, ResourceIndex, RiModel, REND_INST};
use wt_unpack::texlib::TextureLibrary;
use wt_unpack::vehicle;

const LOD_DISTANCE: f32 = 110.0;
const MAX_TRIS: usize = 900;

type Tri = [[f32; 3]; 3];
type Tm = [[f32; 3]; 4];

#[derive(Default, Clone)]
struct Mesh {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    rgba: Vec<[u8; 4]>,
    idx: Vec<u32>,
}

impl Mesh {
    fn push_quad(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3], n: [f32; 3], col: [u8; 4]) {
        let base = self.pos.len() as u32;
        self.pos.extend_from_slice(&[a, b, c, d]);
        self.nrm.extend_from_slice(&[n, n, n, n]);
        self.rgba.extend_from_slice(&[col, col, col, col]);
        // Two triangles: (a, b, c) and (a, c, d)
        self.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn push_tri(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], n: [f32; 3], col: [u8; 4]) {
        let base = self.pos.len() as u32;
        self.pos.extend_from_slice(&[a, b, c]);
        self.nrm.extend_from_slice(&[n, n, n]);
        self.rgba.extend_from_slice(&[col, col, col]);
        self.idx.extend_from_slice(&[base, base + 1, base + 2]);
    }

    fn push_flat_rect(&mut self, x0: f32, z0: f32, x1: f32, z1: f32, y: f32, col: [u8; 4]) {
        let (xa, xb) = (x0.min(x1), x0.max(x1));
        let (za, zb) = (z0.min(z1), z0.max(z1));
        self.push_quad([xa, y, za], [xa, y, zb], [xb, y, zb], [xb, y, za], [0.0, 1.0, 0.0], col);
    }

    fn push_box(&mut self, x0: f32, y0: f32, z0: f32, x1: f32, y1: f32, z1: f32, top_col: [u8; 4], side_col: [u8; 4]) {
        // Top
        self.push_quad([x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [0.0, 1.0, 0.0], top_col);
        // +X
        self.push_quad([x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1], [1.0, 0.0, 0.0], side_col);
        // -X
        self.push_quad([x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0], [-1.0, 0.0, 0.0], side_col);
        // +Z
        self.push_quad([x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1], [0.0, 0.0, 1.0], side_col);
        // -Z
        self.push_quad([x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0], [0.0, 0.0, -1.0], side_col);
    }
}

#[derive(Serialize)]
struct PrefabOut {
    hx: f32,
    hz: f32,
    runway_half_len: f32,
    spawns: Vec<[f32; 2]>,
    inst: Vec<(u32, [f32; 12])>,
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / l, v[1] / l, v[2] / l]
}

fn face_normal(t: &Tri) -> [f32; 3] {
    let u = [t[1][0] - t[0][0], t[1][1] - t[0][1], t[1][2] - t[0][2]];
    let v = [t[2][0] - t[0][0], t[2][1] - t[0][1], t[2][2] - t[0][2]];
    normalize3([u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]])
}

fn add_h_pad(m: &mut Mesh, cx: f32, cz: f32, half: f32, y_base: f32, snow: bool) {
    let conc = if snow { [178, 185, 194, 255] } else { [162, 165, 168, 255] };
    let side = [118, 120, 122, 255];
    let tarmac = if snow { [88, 94, 102, 255] } else { [74, 78, 84, 255] };
    let yellow = [238, 196, 62, 255];
    let white = [236, 240, 244, 255];

    // Raised concrete pad slab + dark tarmac touchdown square
    m.push_box(cx - half, y_base - 0.06, cz - half, cx + half, y_base, cz + half, conc, side);
    let inner = half * 0.78;
    let y1 = y_base + 0.018;
    m.push_flat_rect(cx - inner, cz - inner, cx + inner, cz + inner, y1, tarmac);

    // Yellow perimeter touchdown frame
    let y2 = y_base + 0.035;
    let b = (half * 0.08).max(0.45);
    let f = inner * 0.88;
    m.push_flat_rect(cx - f, cz - f, cx + f, cz - f + b, y2, yellow);
    m.push_flat_rect(cx - f, cz + f - b, cx + f, cz + f, y2, yellow);
    m.push_flat_rect(cx - f, cz - f + b, cx - f + b, cz + f - b, y2, yellow);
    m.push_flat_rect(cx + f - b, cz - f + b, cx + f, cz + f - b, y2, yellow);

    // Crisp 'H' oriented for pilot approaching / taking off along -X:
    // Left & right legs along X (at cz - h_z and cz + h_z), crossbar along Z (at cx)
    let hx = inner * 0.46;
    let hz = inner * 0.36;
    let stroke = (half * 0.11).max(0.65);
    m.push_flat_rect(cx - hx, cz - hz - stroke * 0.5, cx + hx, cz - hz + stroke * 0.5, y2, white);
    m.push_flat_rect(cx - hx, cz + hz - stroke * 0.5, cx + hx, cz + hz + stroke * 0.5, y2, white);
    m.push_flat_rect(cx - stroke * 0.5, cz - hz, cx + stroke * 0.5, cz + hz, y2, white);

    // Takeoff direction chevron at -X edge pointing toward -X (battlefield direction)
    let tip_x = cx - inner * 0.96;
    let base_x = cx - inner * 0.68;
    let cw = inner * 0.24;
    m.push_tri([tip_x, y2, cz], [base_x, y2, cz + cw], [base_x, y2, cz - cw], [0.0, 1.0, 0.0], yellow);
}

fn build_pad_deck_mesh(uc: &str, hx: f32, hz: f32, rwy_half: f32, spawns: &[[f32; 2]]) -> Mesh {
    let mut m = Mesh::default();
    let is_snow = uc.contains("arctic") || uc.contains("snow");
    let is_mud = uc.contains("mud");

    let base_col: [u8; 3] = if is_snow {
        [208, 217, 226]
    } else if is_mud {
        [108, 100, 86]
    } else {
        [126, 124, 116]
    };
    let curb_col = if is_snow { [176, 186, 198, 255] } else { [152, 150, 143, 255] };
    let skirt_top: [u8; 4] = if is_snow {
        [192, 202, 214, 255]
    } else if is_mud {
        [94, 86, 72, 255]
    } else {
        [108, 103, 91, 255]
    };
    let skirt_bot: [u8; 4] = if is_snow {
        [165, 176, 190, 255]
    } else {
        [84, 80, 70, 255]
    };

    // 1. Subdivided graded terrace top at y = 0.04m (subtle panel tone variation so close-up 3D has crisp ground scale)
    let nx = 20usize;
    let nz = 16usize;
    let y_terrace = 0.04f32;
    for ix in 0..nx {
        let x0 = -hx + (2.0 * hx) * (ix as f32 / nx as f32);
        let x1 = -hx + (2.0 * hx) * ((ix + 1) as f32 / nx as f32);
        for iz in 0..nz {
            let z0 = -hz + (2.0 * hz) * (iz as f32 / nz as f32);
            let z1 = -hz + (2.0 * hz) * ((iz + 1) as f32 / nz as f32);
            let checker = if (ix + iz) % 2 == 0 { 3i16 } else { -3i16 };
            let c = [
                (base_col[0] as i16 + checker).clamp(0, 255) as u8,
                (base_col[1] as i16 + checker).clamp(0, 255) as u8,
                (base_col[2] as i16 + checker).clamp(0, 255) as u8,
                255,
            ];
            m.push_flat_rect(x0, z0, x1, z1, y_terrace, c);
        }
    }

    // Perimeter retaining curb
    let cw = 2.4f32;
    let y_curb = 0.08f32;
    m.push_flat_rect(-hx, -hz, hx, -hz + cw, y_curb, curb_col);
    m.push_flat_rect(-hx, hz - cw, hx, hz, y_curb, curb_col);
    m.push_flat_rect(-hx, -hz + cw, -hx + cw, hz - cw, y_curb, curb_col);
    m.push_flat_rect(hx - cw, -hz + cw, hx, hz - cw, y_curb, curb_col);

    // 2. Sloped 3D Earth/Snow Embankment Skirt around perimeter (drops -24m over 32m outward run)
    let out = 32.0f32;
    let drop = -24.0f32;
    let push_skirt_quad = |m: &mut Mesh, a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3], n: [f32; 3]| {
        let base = m.pos.len() as u32;
        m.pos.extend_from_slice(&[a, b, c, d]);
        m.nrm.extend_from_slice(&[n, n, n, n]);
        m.rgba.extend_from_slice(&[skirt_top, skirt_top, skirt_bot, skirt_bot]);
        m.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    let segs = 16usize;
    for i in 0..segs {
        let f0 = i as f32 / segs as f32;
        let f1 = (i + 1) as f32 / segs as f32;
        let xa = -hx + 2.0 * hx * f0;
        let xb = -hx + 2.0 * hx * f1;
        let oxa = -(hx + out) + 2.0 * (hx + out) * f0;
        let oxb = -(hx + out) + 2.0 * (hx + out) * f1;
        // -Z edge
        push_skirt_quad(&mut m, [xb, y_terrace, -hz], [xa, y_terrace, -hz], [oxa, drop, -hz - out], [oxb, drop, -hz - out], normalize3([0.0, 0.80, -0.60]));
        // +Z edge
        push_skirt_quad(&mut m, [xa, y_terrace, hz], [xb, y_terrace, hz], [oxb, drop, hz + out], [oxa, drop, hz + out], normalize3([0.0, 0.80, 0.60]));

        let za = -hz + 2.0 * hz * f0;
        let zb = -hz + 2.0 * hz * f1;
        let oza = -(hz + out) + 2.0 * (hz + out) * f0;
        let ozb = -(hz + out) + 2.0 * (hz + out) * f1;
        // -X edge
        push_skirt_quad(&mut m, [-hx, y_terrace, za], [-hx, y_terrace, zb], [-hx - out, drop, ozb], [-hx - out, drop, oza], normalize3([-0.60, 0.80, 0.0]));
        // +X edge
        push_skirt_quad(&mut m, [hx, y_terrace, zb], [hx, y_terrace, za], [hx + out, drop, oza], [hx + out, drop, ozb], normalize3([0.60, 0.80, 0.0]));
    }

    // 3. Paved Concrete Parking Apron under spawn clusters + Main Runway Strip along Z=0
    let apron_col = if is_snow { [186, 194, 204, 255] } else { [146, 149, 152, 255] };
    let rwy_col = if is_snow { [112, 118, 126, 255] } else { [86, 90, 96, 255] };
    let taxi_col = if is_snow { [126, 132, 140, 255] } else { [98, 102, 108, 255] };
    let line_white = [235, 238, 242, 255];
    let line_yellow = [232, 190, 58, 255];

    if !spawns.is_empty() {
        let sx0 = spawns.iter().map(|s| s[0]).fold(f32::INFINITY, f32::min).min(-40.0) - 14.0;
        let sx1 = spawns.iter().map(|s| s[0]).fold(f32::NEG_INFINITY, f32::max).max(40.0) + 14.0;
        let sz0 = spawns.iter().map(|s| s[1]).fold(f32::INFINITY, f32::min).min(-18.0) - 14.0;
        let sz1 = spawns.iter().map(|s| s[1]).fold(f32::NEG_INFINITY, f32::max).max(18.0) + 14.0;
        m.push_flat_rect(sx0.max(-hx + 4.0), sz0.max(-hz + 4.0), sx1.min(hx - 4.0), sz1.min(hz - 4.0), 0.075, apron_col);
    }

    // Main takeoff/landing strip along Z=0 (from -rwy_half to +rwy_half)
    let rl = rwy_half.min(hx - 8.0);
    let rw = 13.5f32;
    m.push_flat_rect(-rl, -rw, rl, rw, 0.10, rwy_col);
    // Side edge stripes
    m.push_flat_rect(-rl + 2.0, -rw + 0.6, rl - 2.0, -rw + 1.3, 0.115, line_white);
    m.push_flat_rect(-rl + 2.0, rw - 1.3, rl - 2.0, rw - 0.6, 0.115, line_white);
    // Dashed centerline (skip central [-14..+14] around main command H-pad)
    let mut x = -rl + 14.0;
    while x + 8.0 < rl - 14.0 {
        if x.abs() > 15.0 {
            m.push_flat_rect(x, -0.45, x + 7.5, 0.45, 0.115, line_white);
        }
        x += 14.0;
    }
    // Runway threshold piano keys at both ends
    for end_sign in [-1.0f32, 1.0f32] {
        let tx0 = if end_sign < 0.0 { -rl + 2.5 } else { rl - 9.5 };
        for k in -3..=3 {
            if k == 0 {
                continue;
            }
            let zc = k as f32 * 3.1;
            m.push_flat_rect(tx0, zc - 0.85, tx0 + 7.0, zc + 0.85, 0.115, line_white);
        }
    }

    // Taxiway connectors from each spawn pad perpendicular to main strip (or nearest row)
    for &[sx, sz] in spawns {
        if sz.abs() > rw + 2.0 {
            let (za, zb) = if sz > 0.0 { (rw, sz) } else { (sz, -rw) };
            m.push_flat_rect(sx - 3.6, za, sx + 3.6, zb, 0.092, taxi_col);
            m.push_flat_rect(sx - 0.28, za, sx + 0.28, zb, 0.106, line_yellow);
        }
    }

    // 4. Central Primary Command [H] Pad at (0, 0) (where '1队近H1' button places helicopter A!)
    add_h_pad(&mut m, 0.0, 0.0, 10.5, 0.16, is_snow);

    // 5. Individual [H] Helicopter Spawn Pads at all 16 real War Thunder `spawn:p3` slots
    for &[sx, sz] in spawns {
        if sx.hypot(sz) > 16.0 {
            add_h_pad(&mut m, sx, sz, 7.2, 0.15, is_snow);
        }
    }

    m
}

fn build_truck_proxy(name: &str) -> Mesh {
    let mut m = Mesh::default();
    let low = name.to_lowercase();
    let is_oil = low.contains("oil");
    let is_van = low.contains("uaz");
    let body_col = if low.contains("us_") || low.contains("studebaker") || low.contains("dodge") {
        [96, 104, 78, 255]
    } else {
        [88, 98, 76, 255]
    };
    let dark = [46, 48, 52, 255];
    if is_van {
        m.push_box(-2.2, 0.35, -1.0, 2.2, 2.05, 1.0, body_col, body_col);
        m.push_box(-2.0, 0.0, -1.05, 2.0, 0.45, 1.05, dark, dark);
    } else {
        // Truck chassis + cab + cargo/fuel tank
        m.push_box(-3.4, 0.0, -1.2, 3.4, 0.65, 1.2, dark, dark);
        m.push_box(1.2, 0.65, -1.15, 3.2, 2.45, 1.15, body_col, body_col);
        let cargo_col = if is_oil { [138, 134, 118, 255] } else { [112, 118, 92, 255] };
        m.push_box(-3.3, 0.65, -1.2, 1.05, 2.55, 1.2, cargo_col, cargo_col);
    }
    m
}

struct Extractor {
    index: ResourceIndex,
    textures: TextureLibrary,
    descs: HashMap<String, Block>,
    tex_colour: HashMap<String, Option<[u8; 4]>>,
    model_by_name: HashMap<String, u32>,
    models: Vec<(String, Mesh)>,
}

fn resource(index: &ResourceIndex, class: u32, name: &str) -> Result<Option<Vec<u8>>> {
    let Some(path) = index.find(class, name) else { return Ok(None) };
    let file = vehicle::map(path)?;
    let pack = GrpPack::parse(&file).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
    Ok(pack.find(name, class).map(|r| pack.data(r).to_vec()))
}

fn box_tris(n: &CollisionNode) -> Vec<Tri> {
    let [lo, hi] = n.bbox;
    let c = |i: usize| [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }];
    let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    quads.iter().flat_map(|q| [[c(q[0]), c(q[1]), c(q[2])], [c(q[0]), c(q[2]), c(q[3])]]).collect()
}

fn collision_tris(index: &ResourceIndex, name: &str) -> Result<Vec<Tri>> {
    let resource_name = format!("{name}_collision");
    let Some(data) = resource(index, COLLISION, &resource_name)? else { return Ok(Vec::new()) };
    let c = Collision::parse(&resource_name, &data)?;
    let mut all = Vec::new();
    for n in &c.nodes {
        let local: Vec<Tri> = match n.kind {
            NodeKind::Box => box_tris(n),
            NodeKind::Mesh | NodeKind::Convex => n.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| n.positions[t[k] as usize])).collect(),
            _ => continue,
        };
        all.extend(local.into_iter().map(|t| t.map(|p| n.to_object(p))));
    }
    Ok(all)
}

fn str_param<'a>(b: &'a Block, key: &str) -> Option<&'a str> {
    b.params.iter().find(|(n, _)| n == key).and_then(|(_, v)| if let Value::Str(s) = v { Some(s.as_str()) } else { None })
}

fn materials(desc: Option<&Block>) -> Vec<(String, Option<String>)> {
    let Some(desc) = desc else { return Vec::new() };
    let tex: Vec<&str> = desc
        .blocks
        .iter()
        .find(|b| b.name == "tex")
        .map(|b| b.params.iter().filter_map(|(_, v)| if let Value::Str(s) = v { Some(s.as_str()) } else { None }).collect())
        .unwrap_or_default();
    let Some(mats) = desc.blocks.iter().find(|b| b.name == "mat") else { return Vec::new() };
    mats.blocks
        .iter()
        .map(|m| {
            let class = str_param(m, "cls").unwrap_or("").to_string();
            let colour = m.params.iter().find(|(n, _)| n == "t0").and_then(|(_, v)| if let Value::Int(i) = v { tex.get(*i as usize) } else { None }).map(|s| s.to_string());
            (class, colour)
        })
        .collect()
}

impl Extractor {
    fn colour(&mut self, name: &str) -> Option<[u8; 4]> {
        if let Some(c) = self.tex_colour.get(name) {
            return *c;
        }
        let c = self.textures.decode(name, 64).ok().flatten().and_then(|img| {
            let (mut acc, mut wsum) = ([0f64; 3], 0f64);
            let opaque = img.rgba.chunks_exact(4).any(|p| p[3] > 160);
            for p in img.rgba.chunks_exact(4) {
                if opaque && p[3] <= 160 {
                    continue;
                }
                for k in 0..3 {
                    acc[k] += f64::from(p[k]);
                }
                wsum += 1.0;
            }
            (wsum > 0.0).then(|| [(acc[0] / wsum) as u8, (acc[1] / wsum) as u8, (acc[2] / wsum) as u8, 255])
        });
        self.tex_colour.insert(name.to_string(), c);
        c
    }

    fn render_ri_mesh(&mut self, model: &RiModel, mats: &[(String, Option<String>)]) -> Option<Mesh> {
        let last = if model.has_impostor { model.lods.len().checked_sub(2)? } else { model.lods.len().checked_sub(1)? };
        let mut lod = model.lod_for_distance(LOD_DISTANCE).min(last);
        while lod < last && model.triangles(lod) > MAX_TRIS {
            lod += 1;
        }
        let mut m = Mesh::default();
        let mut remap: HashMap<(usize, usize, u32), u32> = HashMap::new();
        for e in &model.lods.get(lod)?.elems {
            let (class, tex) = mats.get(e.material).cloned().unwrap_or_default();
            if class.contains("decal") || class.contains("baked_impostor") || is_facing_leaves(&class) {
                continue;
            }
            let rgba = tex.and_then(|t| self.colour(&t)).unwrap_or([152, 148, 138, 255]);
            let vd = &model.vdata[e.vdata];
            let Some(range) = vd.indices.get(e.first_index..e.first_index + e.triangles * 3) else { continue };
            let mut tri = Vec::with_capacity(3);
            for &raw in range {
                let v = raw as usize + e.base_vertex;
                let key = (e.material, e.vdata, v as u32);
                let i = match remap.get(&key) {
                    Some(&i) => i,
                    None => {
                        let Ok(vx) = model.vertex(vd, v) else { break };
                        let n = vx.normal;
                        let idx = m.pos.len() as u32;
                        m.pos.push(vx.pos);
                        m.nrm.push(n);
                        m.rgba.push(rgba);
                        remap.insert(key, idx);
                        idx
                    }
                };
                tri.push(i);
                if tri.len() == 3 {
                    m.idx.extend_from_slice(&tri);
                    tri.clear();
                }
            }
        }
        (!m.idx.is_empty()).then_some(m)
    }

    fn get_or_extract(&mut self, name: &str) -> Option<u32> {
        if let Some(&id) = self.model_by_name.get(name) {
            return Some(id);
        }
        let model = resource(&self.index, REND_INST, name).ok().flatten().and_then(|d| RiModel::parse(&d).ok());
        let mats = materials(self.descs.get(name));
        let mut mesh = model.as_ref().and_then(|m| self.render_ri_mesh(m, &mats));
        let mut src_tag = "RiModel";
        if mesh.is_none() {
            if let Ok(all) = collision_tris(&self.index, name) {
                if !all.is_empty() {
                    src_tag = "Collision";
                    mesh = Some(Mesh {
                        pos: all.iter().flatten().copied().collect(),
                        nrm: all.iter().flat_map(|t| { let n = face_normal(t); [n, n, n] }).collect(),
                        rgba: vec![[148, 144, 134, 255]; all.len() * 3],
                        idx: (0..all.len() as u32 * 3).collect(),
                    });
                }
            }
        }
        if mesh.is_none() && (name.contains("uaz") || name.contains("studebaker") || name.contains("m35") || name.contains("dodge") || name.contains("truck")) {
            src_tag = "VehicleProxy";
            mesh = Some(build_truck_proxy(name));
        }
        let m = mesh?;
        println!("  + model {:38} ({:12}): {:4} verts, {:4} tris", name, src_tag, m.pos.len(), m.idx.len() / 3);
        let id = self.models.len() as u32;
        self.models.push((name.to_string(), m));
        self.model_by_name.insert(name.to_string(), id);
        Some(id)
    }
}

fn parse_blk_prefab(txt: &str) -> (f32, Vec<[f32; 2]>, Vec<(String, Tm)>) {
    let mut rwy_half = 125.0f32;
    let mut spawns = Vec::new();
    let mut nodes = Vec::new();

    for line in txt.lines() {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("start:p3=") {
            let nums: Vec<f32> = rest.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if nums.len() == 3 && nums[1].abs() < 1.0 && nums[2].abs() < 1.0 && nums[0].abs() >= 100.0 {
                rwy_half = nums[0].abs();
            }
        } else if let Some(rest) = s.strip_prefix("spawn:p3=") {
            let nums: Vec<f32> = rest.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if nums.len() == 3 {
                spawns.push([nums[0], nums[2]]);
            }
        }
    }

    for part in txt.split("node{").skip(1) {
        let body = part.split('}').next().unwrap_or("");
        let mut name_opt: Option<String> = None;
        let mut tm_opt: Option<Tm> = None;
        for ln in body.lines() {
            let s = ln.trim();
            if let Some(r) = s.strip_prefix("name:t=\"") {
                if let Some(nm) = r.strip_suffix('"') {
                    name_opt = Some(nm.to_string());
                }
            } else if let Some(r) = s.strip_prefix("tm:m=[") {
                if let Some(inner) = r.strip_suffix(']') {
                    let mut rows = Vec::new();
                    for chunk in inner.split('[') {
                        let c = chunk.trim().trim_end_matches(']').trim();
                        if c.is_empty() {
                            continue;
                        }
                        let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        if v.len() == 3 {
                            rows.push([v[0], v[1], v[2]]);
                        }
                    }
                    if rows.len() == 4 {
                        tm_opt = Some([rows[0], rows[1], rows[2], rows[3]]);
                    }
                }
            }
        }
        if let (Some(nm), Some(tm)) = (name_opt, tm_opt) {
            if !nm.starts_with('^') && !nm.starts_with('+') {
                nodes.push((nm, tm));
            }
        }
    }

    (rwy_half, spawns, nodes)
}

fn pad4(b: &mut Vec<u8>) {
    while b.len() % 4 != 0 {
        b.push(0);
    }
}

fn serialize_wtvo_v3(models: &[(String, Mesh)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"WTVO");
    b.extend_from_slice(&3u32.to_le_bytes());
    // 1x1 empty roof & veg grids (12 bytes each) so standard parse_objects() works directly
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    for _ in 0..2 {
        b.extend_from_slice(&0.0f32.to_le_bytes());
        b.extend_from_slice(&0.05f32.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        pad4(&mut b);
    }
    b.extend_from_slice(&(models.len() as u32).to_le_bytes());
    for (name, m) in models {
        b.extend_from_slice(&(name.len() as u32).to_le_bytes());
        b.extend_from_slice(name.as_bytes());
        pad4(&mut b);
        let (nv, ni) = (m.pos.len() as u32, m.idx.len() as u32);
        b.extend_from_slice(&nv.to_le_bytes());
        b.extend_from_slice(&ni.to_le_bytes());
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &m.pos {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if m.pos.is_empty() {
            (lo, hi) = ([0.0; 3], [0.0; 3]);
        }
        let size: [f32; 3] = std::array::from_fn(|k| (hi[k] - lo[k]).max(1e-6));
        for v in lo.iter().chain(&size) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for p in &m.pos {
            for k in 0..3 {
                let q = (((p[k] - lo[k]) / size[k]) * 65535.0).round().clamp(0.0, 65535.0) as u16;
                b.extend_from_slice(&q.to_le_bytes());
            }
        }
        pad4(&mut b);
        for n in &m.nrm {
            b.extend(n.map(|x| (x.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8));
            b.push(0);
        }
        for c in &m.rgba {
            b.extend_from_slice(c);
        }
        for i in &m.idx {
            b.extend_from_slice(&i.to_le_bytes());
        }
    }
    // 0 embedded instances, 0 tree models, 0 trees (instances are stored per-prefab in JSON)
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b
}

fn default_extents(uc: &str) -> (f32, f32) {
    if uc.contains("beton_b") {
        (260.0, 136.0)
    } else if uc.contains("mud_b") {
        (162.0, 138.0)
    } else if uc.contains("arctic") {
        (158.0, 136.0)
    } else {
        (138.0, 134.0)
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        bail!("usage: helipads <wt game dir> <helipad_defs dir> <out dir>");
    }
    let game = PathBuf::from(&args[1]);
    let defs_dir = PathBuf::from(&args[2]);
    let out_dir = PathBuf::from(&args[3]);
    fs::create_dir_all(&out_dir)?;

    println!("Indexing War Thunder RendInst & textures from {} ...", game.display());
    let mut ext = Extractor {
        index: ResourceIndex::build(&game, &[COLLISION, REND_INST])?,
        textures: TextureLibrary::open(&game)?,
        descs: model_descs(&game)?,
        tex_colour: HashMap::new(),
        model_by_name: HashMap::new(),
        models: Vec::new(),
    };

    let mut blk_files: Vec<PathBuf> = fs::read_dir(&defs_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "blk"))
        .collect();
    blk_files.sort();

    let mut prefabs: HashMap<String, PrefabOut> = HashMap::new();
    let ident: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

    for bf in &blk_files {
        let uc = bf.file_stem().unwrap().to_string_lossy().to_lowercase();
        let txt = fs::read_to_string(bf).with_context(|| format!("reading {}", bf.display()))?;
        let (rwy_half, spawns, nodes) = parse_blk_prefab(&txt);
        let (mut hx, mut hz) = default_extents(&uc);
        for &[sx, sz] in &spawns {
            hx = hx.max(sx.abs() + 14.0);
            hz = hz.max(sz.abs() + 14.0);
        }
        for (_, tm) in &nodes {
            hx = hx.max(tm[3][0].abs() + 12.0);
            hz = hz.max(tm[3][2].abs() + 12.0);
        }
        hx = hx.ceil();
        hz = hz.ceil();

        let deck_name = format!("pad_deck_{uc}");
        let deck_mesh = build_pad_deck_mesh(&uc, hx, hz, rwy_half, &spawns);
        let deck_id = ext.models.len() as u32;
        println!(
            "Prefab {:30} -> pad=({:.0}x{:.0}m), spawns={}, deck={} tris, nodes={}",
            uc,
            hx * 2.0,
            hz * 2.0,
            spawns.len(),
            deck_mesh.idx.len() / 3,
            nodes.len()
        );
        ext.models.push((deck_name, deck_mesh));

        let mut inst = vec![(deck_id, ident)];
        for (nm, tm) in nodes {
            if let Some(mid) = ext.get_or_extract(&nm) {
                let mut flat = [0.0f32; 12];
                for (r, row) in tm.iter().enumerate() {
                    flat[r * 3..r * 3 + 3].copy_from_slice(row);
                }
                inst.push((mid, flat));
            } else {
                println!("  [skip missing node] {nm}");
            }
        }

        prefabs.insert(
            uc,
            PrefabOut {
                hx,
                hz,
                runway_half_len: rwy_half,
                spawns: spawns.into_iter().map(|s| [(s[0] * 10.0).round() / 10.0, (s[1] * 10.0).round() / 10.0]).collect(),
                inst,
            },
        );
    }

    let wtvo = serialize_wtvo_v3(&ext.models);
    fs::write(out_dir.join("models.wtvo"), &wtvo)?;
    fs::write(out_dir.join("prefabs.json"), serde_json::to_string_pretty(&prefabs)?)?;
    println!(
        "Wrote {} shared models ({:.1} KB WTVO v3) + {} prefabs to {}",
        ext.models.len(),
        wtvo.len() as f64 / 1024.0,
        prefabs.len(),
        out_dir.display()
    );
    Ok(())
}
