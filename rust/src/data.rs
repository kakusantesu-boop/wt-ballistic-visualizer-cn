//! Reads the files extractor/extract.py and extractor/tank_models.py write into ../data. Each is a JS call
//! `NAME(arg, arg, ...);` whose arguments are JSON, so the argument list is parsed as one JSON array.

use anyhow::{Context as _, Result, bail, ensure};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

use crate::core::{Grid, Shell, Terrain, TerrainMeta};
pub use crate::store::Store;

fn call_args(store: &Store, rel: &str, name: &str) -> Result<Vec<Value>> {
    let s = store.read(rel)?;
    let s = s.trim();
    let body = s.strip_prefix(name).and_then(|r| r.strip_prefix('(')).with_context(|| format!("{rel} is not a {name}(...) file"))?;
    let body = body.trim_end_matches(';').trim_end().strip_suffix(')').context("unterminated call")?;
    Ok(serde_json::from_str(&format!("[{body}]"))?)
}

fn b64(s: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}
fn inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = vec![];
    flate2::read::ZlibDecoder::new(bytes).read_to_end(&mut out)?;
    Ok(out)
}
/// A zlib + base64 payload, or "@<entry>": the raw bytes kept as their own entry (lite bundle).
fn payload(store: &Store, s: &str) -> Result<Vec<u8>> {
    match s.strip_prefix('@') {
        Some(rel) => store.bytes(rel),
        None => inflate(&b64(s)?),
    }
}
/// A data URL, or "@<entry>": the encoded image as its own entry (lite bundle).
fn image_of(store: &Store, s: &str) -> Result<image::RgbaImage> {
    match s.strip_prefix('@') {
        Some(rel) => Ok(image::load_from_memory(&store.bytes(rel)?)?.to_rgba8()),
        None => data_url_image(s),
    }
}
/// `data:<mime>;base64,<...>` -> decoded image
fn data_url_image(url: &str) -> Result<image::RgbaImage> {
    let (_, data) = url.split_once(";base64,").context("not a base64 data URL")?;
    Ok(image::load_from_memory(&b64(data)?)?.to_rgba8())
}

// ------------------------------------------------------------------ index

#[derive(Deserialize, Clone)]
pub struct Gun {
    pub weapon: String,
    #[serde(default)]
    pub title: Option<String>,
    pub pitch: [f64; 2],
    pub shells: Vec<Shell>,
}
#[derive(Deserialize, Clone)]
pub struct Tank {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub en: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub guns: Vec<Gun>,
}
#[derive(Deserialize, Clone)]
pub struct MapEntry {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub en: Option<String>,
    pub width: usize,
    pub cell: f64,
    #[serde(default)]
    pub objects: bool,
    #[serde(default)]
    pub ground: bool,
}
#[derive(Deserialize)]
pub struct Index {
    pub maps: Vec<MapEntry>,
    pub tanks: Vec<Tank>,
}

pub fn load_index(store: &Store) -> Result<Index> {
    let mut a = call_args(store, "index.js", "WTV_INDEX")?;
    ensure!(!a.is_empty(), "empty index.js");
    Ok(serde_json::from_value(a.remove(0))?)
}

// ------------------------------------------------------------------ map

#[derive(Deserialize, Clone, Debug)]
pub struct Area {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub team: Option<i64>,
    #[serde(rename = "type")]
    pub shape: String,
    pub tm: [[f64; 3]; 4],
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Mission {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    pub areas: Vec<Area>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct MapMeta {
    pub name: String,
    #[serde(default)]
    pub bounds: Option<[f64; 4]>,
    pub width: usize,
    pub height: usize,
    pub cell: f64,
    pub ox: f64,
    pub oz: f64,
    pub hmin: f64,
    pub hmax: f64,
    pub missions: Vec<Mission>,
    #[serde(default)]
    pub water: Option<f64>,
}
impl MapMeta {
    pub fn terrain_meta(&self) -> TerrainMeta {
        TerrainMeta { width: self.width, height: self.height, cell: self.cell, ox: self.ox, oz: self.oz, hmin: self.hmin, hmax: self.hmax }
    }
}

pub fn load_map(store: &Store, name: &str) -> Result<(MapMeta, Vec<u16>)> {
    let mut a = call_args(store, &format!("maps/{name}.js"), "WTV_MAP")?;
    let mut obj = a.pop().context("empty map file")?;
    let h = obj.as_object_mut().and_then(|o| o.remove("h")).and_then(|v| v.as_str().map(str::to_owned)).context("map has no heightmap")?;
    let meta: MapMeta = serde_json::from_value(obj)?;
    let mut raw = payload(store, &h)?;
    if h.ends_with(crate::store::PREDICTED) {
        crate::store::unpredict(&mut raw, meta.width);
    }
    ensure!(raw.len() == meta.width * meta.height * 2, "heightmap size mismatch");
    let h16 = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Ok((meta, h16))
}

// ------------------------------------------------------------------ objects (WTVO v2, see extractor/objects main.rs)

pub struct Model {
    pub pos: Vec<f32>,
    pub nrm: Vec<i8>,  // 4 per vertex
    pub rgba: Vec<u8>, // 4 per vertex
    pub idx: Vec<u32>,
}
pub struct TreeModel {
    pub height: f32,
    pub radius: f32,
    pub crown: f32,
    pub rgba: [u8; 4],
}
pub struct Objects {
    pub roof: Grid,
    pub veg: Grid,
    pub models: Vec<Model>,
    /// per instance: model index, then 12 floats (rows of a Dagor tm: x axis, y axis, z axis, position)
    pub inst: Vec<(u32, [f32; 12])>,
    pub tree_models: Vec<TreeModel>,
    /// per tree: tree model index, x, y, z, scale
    pub trees: Vec<(u32, [f32; 4])>,
}

struct Rd<'a> {
    b: &'a [u8],
    o: usize,
}
impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(self.o + n <= self.b.len(), "objects file truncated");
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn align4(&mut self) {
        self.o = (self.o + 3) & !3;
    }
    fn f32s(&mut self, n: usize) -> Result<Vec<f32>> {
        Ok(self.take(n * 4)?.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect())
    }
}

pub fn parse_objects(buf: &[u8]) -> Result<Objects> {
    let mut r = Rd { b: buf, o: 0 };
    let (magic, version) = (r.u32()?, r.u32()?);
    ensure!(magic == 0x4f56_5457 && (version == 2 || version == 3), "not a WTVO v2/v3 file: re-run extractor/extract.py");
    let (w, h) = (r.u32()? as usize, r.u32()? as usize);
    let grid = |r: &mut Rd| -> Result<Grid> {
        let (base, scale) = (r.f32()?, r.f32()?);
        let q = r.take(w * h * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        r.align4();
        Ok(Grid { base, scale, q })
    };
    let roof = grid(&mut r)?;
    let veg = grid(&mut r)?;
    let mut models = vec![];
    for _ in 0..r.u32()? {
        let len = r.u32()? as usize;
        r.take(len)?;
        r.align4();
        let (nv, ni) = (r.u32()? as usize, r.u32()? as usize);
        let pos = if version == 3 {
            // v3 (lite bundle): u16 positions over the model's bounding box, see store::quantize_objects
            let (lo, size) = (r.f32s(3)?, r.f32s(3)?);
            let q = r.take(nv * 6)?.chunks_exact(2).enumerate().map(|(i, c)| lo[i % 3] + u16::from_le_bytes([c[0], c[1]]) as f32 / 65535.0 * size[i % 3]).collect();
            r.align4();
            q
        } else {
            r.f32s(nv * 3)?
        };
        let nrm = r.take(nv * 4)?.iter().map(|&b| b as i8).collect();
        let rgba = r.take(nv * 4)?.to_vec();
        let idx = r.take(ni * 4)?.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        models.push(Model { pos, nrm, rgba, idx });
    }
    let ni = r.u32()? as usize;
    let raw = r.take(ni * 52)?;
    let inst = raw
        .chunks_exact(52)
        .map(|c| {
            let m = u32::from_le_bytes(c[0..4].try_into().unwrap());
            let mut f = [0f32; 12];
            for (k, v) in f.iter_mut().enumerate() {
                *v = f32::from_le_bytes(c[4 + k * 4..8 + k * 4].try_into().unwrap());
            }
            (m, f)
        })
        .collect();
    let mut tree_models = vec![];
    for _ in 0..r.u32()? {
        let (height, radius, crown) = (r.f32()?, r.f32()?, r.f32()?);
        let rgba = r.take(4)?.try_into()?;
        tree_models.push(TreeModel { height, radius, crown, rgba });
    }
    let nt = r.u32()? as usize;
    let trees = r
        .take(nt * 20)?
        .chunks_exact(20)
        .map(|c| {
            let f = |k: usize| f32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap());
            (u32::from_le_bytes(c[0..4].try_into().unwrap()), [f(1), f(2), f(3), f(4)])
        })
        .collect();
    Ok(Objects { roof, veg, models, inst, tree_models, trees })
}

pub fn load_objects(store: &Store, name: &str) -> Result<Objects> {
    let a = call_args(store, &format!("maps/{name}.obj.js"), "WTV_OBJ")?;
    let b = a.get(1).and_then(Value::as_str).context("objects file without data")?;
    let o = parse_objects(&payload(store, b)?)?;
    Ok(o)
}

// ------------------------------------------------------------------ ground colours

/// idx: which detail texture each ground texel uses (red channel, 255 none); strip: the detail textures side by side.
pub struct Detail {
    pub idx: image::RgbaImage,
    pub strip: image::RgbaImage,
    pub tiles: Vec<f32>,
}
pub struct Ground {
    pub color: image::RgbaImage,
    pub detail: Option<Detail>,
}

pub fn load_ground(store: &Store, name: &str) -> Result<Ground> {
    let a = call_args(store, &format!("maps/{name}.tex.js"), "WTV_TEX")?;
    let color = image_of(store, a.get(1).and_then(Value::as_str).context("ground file without image")?)?;
    let detail = match a.get(2) {
        Some(Value::Object(d)) => Some(Detail {
            idx: image_of(store, d.get("idx").and_then(Value::as_str).context("detail idx")?)?,
            strip: image_of(store, d.get("strip").and_then(Value::as_str).context("detail strip")?)?,
            tiles: serde_json::from_value(d.get("tiles").cloned().context("detail tiles")?)?,
        }),
        _ => None,
    };
    Ok(Ground { color, detail })
}

// ------------------------------------------------------------------ tank models (WTVK v1, see extractor/tank_models.py)

/// Flat-shaded triangles: 3 floats position and 3 floats normal per vertex, +X front, +Y up, lowest point y = 0.
pub struct TankMesh {
    pub pos: Vec<f32>,
    pub nrm: Vec<f32>,
}

pub fn load_tank(store: &Store, model: &str) -> Result<TankMesh> {
    let a = call_args(store, &format!("tanks/{model}.js"), "WTV_TANK")?;
    let buf = payload(store, a.get(1).and_then(Value::as_str).context("tank file without data")?)?;
    let mut r = Rd { b: &buf, o: 0 };
    if r.u32()? != 0x4b56_5457 || r.u32()? != 1 {
        bail!("not a WTVK v1 file");
    }
    let (nv, nt) = (r.u32()? as usize, r.u32()? as usize);
    let q: Vec<f32> = r.take(nv * 6)?.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 1000.0).collect();
    let idx: Vec<usize> = r.take(nt * 6)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as usize).collect();
    let (mut pos, mut nrm) = (Vec::with_capacity(nt * 9), Vec::with_capacity(nt * 9));
    for t in idx.chunks_exact(3) {
        ensure!(t.iter().all(|&i| i < nv), "tank index out of range");
        let p: Vec<[f32; 3]> = t.iter().map(|&i| [q[i * 3], q[i * 3 + 1], q[i * 3 + 2]]).collect();
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let w = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
        for v in &p {
            pos.extend_from_slice(v);
            nrm.extend(n.iter().map(|c| c / l));
        }
    }
    Ok(TankMesh { pos, nrm })
}

/// A loaded map with the surfaces the page builds: t = bare ground (positions of A, B), tr = ground + roofs,
/// tv = ground + roofs + crowns. tc / ts (shots / sight) pick tr or tv by the two "block" switches.
#[derive(Clone)]
pub struct LoadedMap {
    pub meta: MapMeta,
    pub h16: Vec<u16>,
    pub t: Terrain,
    pub tr: Terrain,
    pub tv: Terrain,
    pub obj: Option<std::sync::Arc<Objects>>,
    pub ground: Option<std::sync::Arc<Ground>>,
}

impl LoadedMap {
    pub fn new(meta: MapMeta, h16: Vec<u16>) -> Self {
        let t = Terrain::new(&meta.terrain_meta(), &h16, &[]);
        Self { tr: t.clone(), tv: t.clone(), t, meta, h16, obj: None, ground: None }
    }
    /// Where a tank at (x, z) stands: the ground, or the top of a rock or other solid object there.
    pub fn stand(&self, x: f64, z: f64) -> f64 {
        self.tr.height_at(x, z)
    }
    pub fn set_objects(&mut self, o: Objects) -> Result<()> {
        ensure!(o.roof.q.len() == self.meta.width * self.meta.height, "object grid does not match the heightmap");
        let tm = self.meta.terrain_meta();
        self.tr = Terrain::new(&tm, &self.h16, &[&o.roof]);
        self.tv = Terrain::new(&tm, &self.h16, &[&o.roof, &o.veg]);
        self.obj = Some(std::sync::Arc::new(o));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_real_data() {
        let dir = Store::open();
        let Ok(idx) = load_index(&dir) else { return }; // no extracted data in this checkout
        assert!(idx.maps.len() > 10 && idx.tanks.len() > 1000);
        let m = &idx.maps[0];
        let (meta, h16) = load_map(&dir, &m.name).unwrap();
        assert_eq!(h16.len(), meta.width * meta.height);
        if m.objects {
            let o = load_objects(&dir, &m.name).unwrap();
            assert!(!o.models.is_empty() && !o.inst.is_empty());
        }
        if m.ground {
            assert!(load_ground(&dir, &m.name).unwrap().color.width() > 0);
        }
        let t = load_tank(&dir, "t_72a").unwrap();
        assert!(t.pos.len() > 1000);
    }

    #[test]
    fn quantized_objects_match() {
        let store = Store::open();
        let Ok(a) = call_args(&store, "maps/avg_normandy.obj.js", "WTV_OBJ") else { return };
        let v2 = inflate(&b64(a[1].as_str().unwrap()).unwrap()).unwrap();
        let (o2, o3) = (parse_objects(&v2).unwrap(), parse_objects(&crate::store::quantize_objects(&v2).unwrap()).unwrap());
        assert_eq!((o2.models.len(), o2.inst.len(), o2.trees.len()), (o3.models.len(), o3.inst.len(), o3.trees.len()));
        assert_eq!(o2.roof.q, o3.roof.q);
        for (m2, m3) in o2.models.iter().zip(&o3.models) {
            assert_eq!((m2.idx.len(), &m2.nrm, &m2.rgba), (m3.idx.len(), &m3.nrm, &m3.rgba));
            for (p2, p3) in m2.pos.iter().zip(&m3.pos) {
                assert!((p2 - p3).abs() < 0.01, "{p2} vs {p3}");
            }
        }
    }
}
