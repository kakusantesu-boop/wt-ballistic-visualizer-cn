//! Where the data files come from: a pack appended to the executable (a self-contained build made with
//! `cargo run --release --bin bundle`), else a `data` folder. Paths are relative to data/, e.g. "maps/avg_berlin.js".
//!
//! Pack layout, after the executable's own bytes: each entry compressed, then a JSON index
//! [[path, offset, length, codec], ...] (offsets from the start of the file; codec 1 = xz), then the footer:
//! u64 index offset, u64 index length (little endian), b"WTVPACK2".
//! Big payloads are their own entries; the data files point at them with "@<path>" (see src/bin/bundle.rs).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

const MAGIC: &[u8; 8] = b"WTVPACK2";

pub enum Store {
    Dir(PathBuf),
    Pack { file: PathBuf, files: HashMap<String, (u64, u64, u8)> },
}

impl Store {
    /// The pack in this executable, else `WTV_DATA`, else `data` next to the executable, else the source tree's data/.
    pub fn open() -> Store {
        let exe = std::env::current_exe().ok();
        if let Some(files) = exe.as_deref().and_then(|e| read_index(e).ok().flatten()) {
            return Store::Pack { file: exe.unwrap(), files };
        }
        if let Ok(d) = std::env::var("WTV_DATA") {
            return Store::Dir(d.into());
        }
        let beside = exe.as_deref().and_then(Path::parent).map(|d| d.join("data"));
        match beside {
            Some(d) if d.join("index.js").exists() => Store::Dir(d),
            _ => Store::Dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../data")), // cargo run in the source tree
        }
    }

    pub fn bytes(&self, rel: &str) -> Result<Vec<u8>> {
        match self {
            Store::Dir(d) => std::fs::read(d.join(rel)).with_context(|| format!("missing {}", d.join(rel).display())),
            Store::Pack { file, files } => {
                let &(off, len, codec) = files.get(rel).with_context(|| format!("{rel} is not in the bundled data"))?;
                let mut f = File::open(file)?;
                f.seek(SeekFrom::Start(off))?;
                let mut z = vec![0u8; len as usize];
                f.read_exact(&mut z)?;
                let mut out = vec![];
                ensure!(codec == 1, "{rel}: unknown codec {codec}");
                liblzma::read::XzDecoder::new(&z[..]).read_to_end(&mut out)?;
                Ok(out)
            }
        }
    }

    pub fn read(&self, rel: &str) -> Result<String> {
        String::from_utf8(self.bytes(rel)?).with_context(|| format!("{rel} is not text"))
    }

    /// For error messages.
    pub fn describe(&self) -> String {
        match self {
            Store::Dir(d) => format!("data folder {}", d.display()),
            Store::Pack { files, .. } => format!("bundled data ({} files)", files.len()),
        }
    }

    #[cfg(test)]
    pub fn open_pack(path: &Path) -> Result<Option<Store>> {
        Ok(read_index(path)?.map(|files| Store::Pack { file: path.to_path_buf(), files }))
    }
}

/// The pack index of a file, None when it has no pack.
fn read_index(path: &Path) -> Result<Option<HashMap<String, (u64, u64, u8)>>> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    if size < 24 {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(size - 24))?;
    let mut foot = [0u8; 24];
    f.read_exact(&mut foot)?;
    if &foot[16..] != MAGIC {
        return Ok(None);
    }
    let (off, len) = (u64::from_le_bytes(foot[..8].try_into()?), u64::from_le_bytes(foot[8..16].try_into()?));
    ensure!(off + len + 24 == size, "damaged data pack");
    f.seek(SeekFrom::Start(off))?;
    let mut idx = vec![0u8; len as usize];
    f.read_exact(&mut idx)?;
    let list: Vec<(String, u64, u64, u8)> = serde_json::from_slice(&idx)?;
    Ok(Some(list.into_iter().map(|(p, o, n, c)| (p, (o, n, c))).collect()))
}

/// Write `out` = `exe` + the entries `make` gives for each job (in parallel, a batch at a time to bound memory),
/// packed with xz, + index + footer. Returns (entries, bytes).
#[allow(dead_code)] // used by src/bin/bundle.rs
pub fn bundle<M>(exe: &Path, out: &Path, jobs: &[String], make: M, progress: impl Fn(usize, usize)) -> Result<(usize, u64)>
where
    M: Fn(&str) -> Result<Vec<(String, Vec<u8>)>> + Sync,
{
    use rayon::prelude::*;
    ensure!(read_index(exe)?.is_none(), "{} already carries data; bundle the plain build", exe.display());
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = out.with_extension("partial");
    let mut w = std::io::BufWriter::new(File::create(&tmp)?);
    let mut pos = std::io::copy(&mut File::open(exe)?, &mut w)?;
    let mut index = vec![];
    let batch_len = 8; // 8 xz encoders at a time stay near 5.5 GB
    for (bi, batch) in jobs.chunks(batch_len).enumerate() {
        let packed: Vec<Result<Vec<(String, Vec<u8>)>>> = batch
            .par_iter()
            .map(|job| {
                make(job)?
                    .into_iter() // one encoder per job: xz at level 9 holds ~670 MB
                    .map(|(path, raw)| {
                        let mut e = liblzma::write::XzEncoder::new(Vec::new(), 9);
                        e.write_all(&raw)?;
                        Ok((path, e.finish()?))
                    })
                    .collect()
            })
            .collect();
        for entries in packed {
            for (path, z) in entries? {
                w.write_all(&z)?;
                index.push((path, pos, z.len() as u64, 1u8));
                pos += z.len() as u64;
            }
        }
        progress(((bi + 1) * batch_len).min(jobs.len()), jobs.len());
    }
    let idx = serde_json::to_vec(&index)?;
    w.write_all(&idx)?;
    w.write_all(&pos.to_le_bytes())?;
    w.write_all(&(idx.len() as u64).to_le_bytes())?;
    w.write_all(MAGIC)?;
    w.flush()?;
    drop(w);
    std::fs::rename(&tmp, out)?;
    Ok((index.len(), std::fs::metadata(out)?.len()))
}

// ------------------------------------------------------------------ bundled payload forms

/// Entry name ending of heightmaps stored as prediction residuals (they pack far smaller than the heights).
pub const PREDICTED: &str = ".hpred";

/// Gradient predictor over u16 little-endian rows of width `w`: each height minus (left + up - up-left), wrapping;
/// missing neighbours count as 0. Lossless; `unpredict` undoes it.
#[allow(dead_code)] // used by src/bin/bundle.rs
pub fn predict(b: &mut [u8], w: usize) {
    let h: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    for i in 0..h.len() {
        let x = i % w;
        let l = if x > 0 { h[i - 1] } else { 0 };
        let u = if i >= w { h[i - w] } else { 0 };
        let ul = if x > 0 && i > w { h[i - w - 1] } else { 0 };
        let r = h[i].wrapping_sub(l.wrapping_add(u).wrapping_sub(ul));
        b[i * 2..i * 2 + 2].copy_from_slice(&r.to_le_bytes());
    }
}
#[allow(dead_code)] // the bundler only writes residuals
pub fn unpredict(b: &mut [u8], w: usize) {
    let get = |b: &[u8], i: usize| u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]);
    for i in 0..b.len() / 2 {
        let x = i % w;
        let l = if x > 0 { get(b, i - 1) } else { 0 };
        let u = if i >= w { get(b, i - w) } else { 0 };
        let ul = if x > 0 && i > w { get(b, i - w - 1) } else { 0 };
        let v = get(b, i).wrapping_add(l.wrapping_add(u).wrapping_sub(ul));
        b[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
}

/// WTVO v2 objects (see extractor/objects) to v3: each model's positions as u16 over its bounding box
/// (lo and size as f32 before them), padded to 4 bytes; everything else unchanged. About 3 mm on a 200 m model.
#[allow(dead_code)] // used by src/bin/bundle.rs
pub fn quantize_objects(v2: &[u8]) -> Result<Vec<u8>> {
    let u32_at = |o: usize| -> Result<usize> { Ok(u32::from_le_bytes(v2.get(o..o + 4).context("objects file truncated")?.try_into()?) as usize) };
    ensure!(u32_at(0)? == 0x4f56_5457 && u32_at(4)? == 2, "not a WTVO v2 file");
    let (w, h) = (u32_at(8)?, u32_at(12)?);
    let mut o = 16;
    for _ in 0..2 {
        o = (o + 8 + w * h * 2 + 3) & !3; // grid: base, scale, u16 cells
    }
    let mut out = v2[..o].to_vec();
    out[4..8].copy_from_slice(&3u32.to_le_bytes());
    let n = u32_at(o)?;
    out.extend_from_slice(&v2[o..o + 4]);
    o += 4;
    for _ in 0..n {
        let len = u32_at(o)?;
        let head = (o + 4 + len + 3) & !3;
        let (nv, ni) = (u32_at(head)?, u32_at(head + 4)?);
        out.extend_from_slice(v2.get(o..head + 8).context("objects file truncated")?);
        o = head + 8;
        let pos: Vec<f32> = v2.get(o..o + nv * 12).context("objects file truncated")?.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in pos.chunks_exact(3) {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if nv == 0 {
            (lo, hi) = ([0.0; 3], [0.0; 3]);
        }
        let size: [f32; 3] = std::array::from_fn(|k| (hi[k] - lo[k]).max(1e-6));
        for v in lo.iter().chain(&size) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for p in pos.chunks_exact(3) {
            for k in 0..3 {
                out.extend_from_slice(&(((p[k] - lo[k]) / size[k] * 65535.0).round() as u16).to_le_bytes());
            }
        }
        out.resize((out.len() + 3) & !3, 0);
        o += nv * 12;
        out.extend_from_slice(v2.get(o..o + nv * 8 + ni * 4).context("objects file truncated")?); // normals, colours, indices
        o += nv * 8 + ni * 4;
    }
    out.extend_from_slice(&v2[o..]); // instances, tree models, trees
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn predictor_round_trip() {
        let orig: Vec<u8> = [0u16, 65535, 3, 1000, 7, 7, 65000, 2, 9, 40000, 12, 1].iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut b = orig.clone();
        super::predict(&mut b, 4);
        assert_ne!(b, orig);
        super::unpredict(&mut b, 4);
        assert_eq!(b, orig);
    }

    use super::*;

    #[test]
    fn bundle_round_trip() {
        let dir = std::env::temp_dir().join(format!("wtv_pack_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.exe"), b"MZ fake program").unwrap();
        let jobs = vec!["index.js".to_string(), "maps/a.js".to_string()];
        let make = |r: &str| Ok(vec![(r.to_string(), r.repeat(500).into_bytes()), (format!("{r}.blob"), vec![7u8; 1000])]);
        let out = dir.join("out/app.exe");
        let (n, _) = bundle(&dir.join("app.exe"), &out, &jobs, make, |_, _| {}).unwrap();
        assert_eq!(n, 4);
        let s = Store::Pack { file: out.clone(), files: read_index(&out).unwrap().unwrap() };
        assert_eq!(s.read("maps/a.js").unwrap(), "maps/a.js".repeat(500));
        assert_eq!(s.bytes("index.js.blob").unwrap(), vec![7u8; 1000]);
        assert!(std::fs::read(&out).unwrap().starts_with(b"MZ fake program"));
        assert!(read_index(&dir.join("app.exe")).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
