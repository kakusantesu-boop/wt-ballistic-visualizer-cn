//! Ballistics and terrain core: point-mass flight, the elevation solver, the heightmap surface and the
//! reach / hit / visibility sweeps. Tests at the bottom.

pub const G: f64 = 9.81;
pub const DEG: f64 = std::f64::consts::PI / 180.0;

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
pub struct RocketMotor {
    #[serde(rename = "vStart", default)]
    pub v_start: f64,
    #[serde(default)]
    pub force0: f64,
    #[serde(rename = "tFire0", default)]
    pub t_fire0: f64,
    #[serde(rename = "mEnd0", default)]
    pub m_end0: f64,
    #[serde(default)]
    pub force1: f64,
    #[serde(rename = "tFire1", default)]
    pub t_fire1: f64,
    #[serde(rename = "mEnd1", default)]
    pub m_end1: f64,
    #[serde(rename = "tDelay", default)]
    pub t_delay: f64,
    #[serde(default)]
    pub guided: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Shell {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "typeTitle", default)]
    pub type_title: Option<String>,
    pub v0: f64,
    pub m: f64,
    pub cal: f64,
    pub cx: f64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub en: Option<String>,
    #[serde(rename = "cxEst", default)]
    pub cx_est: bool,
    #[serde(default)]
    pub rocket: Option<RocketMotor>,
}

/// ISA troposphere density (kg/m^3) at altitude y (m)
pub fn rho(y: f64) -> f64 {
    let f = 1.0 - 2.25577e-5 * y;
    1.225 * f.max(0.0).powf(4.2559)
}
/// drag accel = dragK * rho * |v| * v ;  F = 0.5 rho v^2 Cx S,  S = pi d^2 / 4
pub fn drag_k(sh: &Shell) -> f64 {
    0.5 * sh.cx * std::f64::consts::PI * sh.cal * sh.cal / 4.0 / sh.m
}

/// Flight path sampled per time step: s horizontal distance, y absolute altitude, t time, v speed.
#[derive(Clone, Debug, Default)]
pub struct Traj {
    pub s: Vec<f64>,
    pub y: Vec<f64>,
    pub t: Vec<f64>,
    pub v: Vec<f64>,
}

#[derive(Clone, Copy)]
pub enum Key {
    Y,
    T,
    V,
}

impl Traj {
    fn col(&self, k: Key) -> &[f64] {
        match k {
            Key::Y => &self.y,
            Key::T => &self.t,
            Key::V => &self.v,
        }
    }
    /// Linear interpolation of a column at horizontal distance d; NaN outside the path.
    pub fn at(&self, k: Key, d: f64) -> f64 {
        let s = &self.s;
        if d < 0.0 || d > s[s.len() - 1] {
            return f64::NAN;
        }
        let p = idx_at(s, d);
        let q = (p + 1).min(s.len() - 1);
        let w = if s[q] > s[p] { (d - s[p]) / (s[q] - s[p]) } else { 0.0 };
        let c = self.col(k);
        c[p] + (c[q] - c[p]) * w
    }
}

pub struct SimOpt {
    pub dt: f64,
    pub max_t: f64,
    pub y_floor: Option<f64>,
}
impl Default for SimOpt {
    fn default() -> Self {
        Self { dt: 0.004, max_t: 60.0, y_floor: None }
    }
}

/// Point-mass flight in the vertical plane of the shot. Heun (RK2) steps; supports both conventional shells and two-stage rockets.
pub fn simulate(sh: &Shell, elev: f64, y0: f64, max_s: f64, opt: SimOpt) -> Traj {
    let dt = opt.dt;
    let y_floor = opt.y_floor.unwrap_or(y0 - 3000.0);
    if let Some(rm) = &sh.rocket {
        if rm.force0 > 0.0 && rm.t_fire0 > 0.0 {
            let (dir_x, dir_y) = (elev.cos(), elev.sin());
            let c_drag = 0.5 * sh.cx * std::f64::consts::PI * sh.cal * sh.cal / 4.0;
            let t0_start = rm.t_delay.max(0.0);
            let t0_end = t0_start + rm.t_fire0;
            let t1_end = t0_end + rm.t_fire1.max(0.0);
            let m0_end = if rm.m_end0 > 0.0 { rm.m_end0 } else { sh.m };
            let m1_end = if rm.t_fire1 > 0.0 && rm.m_end1 > 0.0 { rm.m_end1 } else { m0_end };
            let stage_at = |tt: f64| -> (f64, f64) {
                if tt < t0_start {
                    (sh.m, 0.0)
                } else if tt < t0_end {
                    let f = (tt - t0_start) / rm.t_fire0;
                    (sh.m + (m0_end - sh.m) * f, rm.force0)
                } else if rm.t_fire1 > 0.0 && tt < t1_end {
                    let f = (tt - t0_end) / rm.t_fire1;
                    (m0_end + (m1_end - m0_end) * f, rm.force1)
                } else {
                    (m1_end, 0.0)
                }
            };
            let accel = |sc: f64, yc: f64, vxc: f64, vyc: f64, tt: f64| -> (f64, f64) {
                let (mass, thrust) = stage_at(tt);
                let spd = vxc.hypot(vyc);
                let (ux, uy) = if spd > 1e-3 { (vxc / spd, vyc / spd) } else { (dir_x, dir_y) };
                let f_drag = c_drag * rho(yc) * spd * spd;
                let a_axial = (thrust - f_drag) / mass.max(1e-3);
                if rm.v_start < 5.0 && sc.hypot(yc - y0) < 2.5 && thrust > 0.0 {
                    let a_rail = (a_axial - G * dir_y).max(0.0);
                    (a_rail * dir_x, a_rail * dir_y)
                } else {
                    (a_axial * ux, a_axial * uy - G)
                }
            };
            let v_init = rm.v_start.max(0.0);
            let (mut s, mut y, mut t) = (0.0, y0, 0.0);
            let (mut vx, mut vy) = (v_init * dir_x, v_init * dir_y);
            let mut tr = Traj { s: vec![0.0], y: vec![y0], t: vec![0.0], v: vec![v_init] };
            while s < max_s && t < opt.max_t && y > y_floor {
                let (ax1, ay1) = accel(s, y, vx, vy, t);
                let (vx2, vy2) = (vx + ax1 * dt, vy + ay1 * dt);
                let (ax2, ay2) = accel(s + vx * dt, y + vy * dt, vx2, vy2, t + dt);
                s += 0.5 * (vx + vx2) * dt;
                y += 0.5 * (vy + vy2) * dt;
                vx += 0.5 * (ax1 + ax2) * dt;
                vy += 0.5 * (ay1 + ay2) * dt;
                t += dt;
                tr.s.push(s);
                tr.y.push(y);
                tr.t.push(t);
                tr.v.push(vx.hypot(vy));
            }
            return tr;
        }
    }
    let k = drag_k(sh);
    let (mut s, mut y, mut t) = (0.0, y0, 0.0);
    let (mut vx, mut vy) = (sh.v0 * elev.cos(), sh.v0 * elev.sin());
    let mut tr = Traj { s: vec![0.0], y: vec![y0], t: vec![0.0], v: vec![sh.v0] };
    while s < max_s && t < opt.max_t && y > y_floor {
        let c1 = k * rho(y) * vx.hypot(vy);
        let (ax1, ay1) = (-c1 * vx, -G - c1 * vy);
        let (vx2, vy2) = (vx + ax1 * dt, vy + ay1 * dt);
        let c2 = k * rho(y + vy * dt) * vx2.hypot(vy2);
        let (ax2, ay2) = (-c2 * vx2, -G - c2 * vy2);
        s += 0.5 * (vx + vx2) * dt;
        y += 0.5 * (vy + vy2) * dt;
        vx += 0.5 * (ax1 + ax2) * dt;
        vy += 0.5 * (ay1 + ay2) * dt;
        t += dt;
        tr.s.push(s);
        tr.y.push(y);
        tr.t.push(t);
        tr.v.push(vx.hypot(vy));
    }
    tr
}

/// last p with S[p] <= d
fn idx_at(s: &[f64], d: f64) -> usize {
    let (mut lo, mut hi) = (0, s.len() - 1);
    while hi - lo > 1 {
        let m = (lo + hi) >> 1;
        if s[m] <= d { lo = m } else { hi = m }
    }
    lo
}

/// Elevations (rad, within pitch limits in deg) whose path passes through (d, y0 + dy). Low arc first.
pub fn solve_exact(sh: &Shell, d: f64, dy: f64, y0: f64, p_min: f64, p_max: f64) -> Vec<f64> {
    let f = |e: f64| {
        let tr = simulate(sh, e, y0, d + 1.0, SimOpt { y_floor: Some(y0 + dy.min(0.0) - 500.0), ..Default::default() });
        tr.at(Key::Y, d) - y0 - dy
    };
    let n = 8.max(((p_max - p_min) / 0.25).ceil() as i64) as usize;
    let mut sols = vec![];
    let (mut pe, mut pv) = (p_min * DEG, f(p_min * DEG));
    for i in 1..=n {
        let e = (p_min + (p_max - p_min) * i as f64 / n as f64) * DEG;
        let v = f(e);
        if pv.is_finite() && v.is_finite() && (pv <= 0.0) != (v <= 0.0) {
            let (mut lo, mut hi, mut flo) = (pe, e, pv);
            for _ in 0..48 {
                let mid = 0.5 * (lo + hi);
                let fm = f(mid);
                if !fm.is_finite() {
                    hi = mid;
                    continue;
                }
                if (fm <= 0.0) == (flo <= 0.0) {
                    lo = mid;
                    flo = fm;
                } else {
                    hi = mid
                }
            }
            sols.push(0.5 * (lo + hi));
        }
        pe = e;
        pv = v;
    }
    sols
}

/// Trajectory family on a uniform distance grid: y[k*n + i] = height above muzzle at s = i*ds for elevation k.
pub struct Table {
    pub y: Vec<f32>,
    pub n: usize,
    pub ne: usize,
    pub ds: f64,
    #[cfg_attr(not(test), allow(dead_code))] // elevation of each row; the tests check rows against it
    pub elev: Vec<f64>,
    pub apex: Vec<usize>,
    pub last: Vec<usize>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_table(sh: &Shell, p_min: f64, p_max: f64, step: f64, y_alt: f64, y_floor_abs: f64, max_s: f64, ds: f64) -> Table {
    let ne = 2.max(((p_max - p_min) / step + 1e-9).floor() as usize + 1);
    let n = (max_s / ds).ceil() as usize + 1;
    let mut y = vec![f32::NAN; ne * n];
    let (mut apex, mut last, mut elev) = (vec![0; ne], vec![0; ne], vec![0.0; ne]);
    for k in 0..ne {
        let e = p_max.min(p_min + k as f64 * step) * DEG;
        elev[k] = e;
        let tr = simulate(sh, e, y_alt, max_s + ds, SimOpt { y_floor: Some(y_floor_abs), dt: 0.004f64.min(ds / sh.v0.max(20.0) * 2.0), ..Default::default() });
        let (s, ty) = (&tr.s, &tr.y);
        let (mut p, mut best, mut bi) = (0, f64::NEG_INFINITY, 0);
        for i in 0..n {
            let si = i as f64 * ds;
            while p + 1 < s.len() && s[p + 1] < si {
                p += 1;
            }
            if p + 1 >= s.len() {
                break;
            }
            let den = s[p + 1] - s[p];
            let w = (si - s[p]) / if den != 0.0 { den } else { 1.0 };
            let yy = ty[p] + (ty[p + 1] - ty[p]) * w - y_alt;
            y[k * n + i] = yy as f32;
            last[k] = i;
            if yy > best {
                best = yy;
                bi = i;
            }
        }
        apex[k] = bi;
    }
    Table { y, n, ne, ds, elev, apex, last }
}

/// Map header fields the terrain needs (see data::MapMeta).
pub struct TerrainMeta {
    pub width: usize,
    pub height: usize,
    pub cell: f64,
    pub ox: f64,
    pub oz: f64,
    pub hmin: f64,
    pub hmax: f64,
}

/// Heights on the same grid as the heightmap (object roofs, tree crowns); q = 0 means none.
#[derive(Clone)]
pub struct Grid {
    pub base: f32,
    pub scale: f32,
    pub q: Vec<u16>,
}

/// Heightmap with Dagor's 4-triangle cell interpolation. With grids, the surface is the max of ground and every
/// grid, and src[i] says which one is on top (0 ground, k = grids[k-1]).
#[derive(Clone)]
pub struct Terrain {
    pub h: Vec<f32>,
    pub w: usize,
    pub hh: usize,
    pub cell: f64,
    pub ox: f64,
    pub oz: f64,
    pub x1: f64,
    pub z1: f64,
    pub lo: f64,
    pub hi: f64,
    src: Option<Vec<u8>>,
}

impl Terrain {
    pub fn new(meta: &TerrainMeta, h16: &[u16], grids: &[&Grid]) -> Self {
        let (w, h, cell) = (meta.width, meta.height, meta.cell);
        let sc = (meta.hmax - meta.hmin) / 65535.0;
        let mut hv: Vec<f32> = h16.iter().map(|&v| (meta.hmin + v as f64 * sc) as f32).collect();
        let src = (!grids.is_empty()).then(|| {
            let mut src = vec![0u8; w * h];
            for (k, gr) in grids.iter().enumerate() {
                for i in 0..hv.len() {
                    let q = gr.q[i];
                    if q != 0 {
                        let y = gr.base + q as f32 * gr.scale;
                        if y > hv[i] {
                            hv[i] = y;
                            src[i] = k as u8 + 1;
                        }
                    }
                }
            }
            src
        });
        let (lo, hi) = hv.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
        Self { h: hv, w, hh: h, cell, ox: meta.ox, oz: meta.oz, x1: meta.ox + (w - 1) as f64 * cell, z1: meta.oz + (h - 1) as f64 * cell, lo: lo as f64, hi: hi as f64, src }
    }
    pub fn src_at(&self, x: f64, z: f64) -> u8 {
        let Some(src) = &self.src else { return 0 };
        let i = (((x - self.ox) / self.cell).round().max(0.0) as usize).min(self.w - 1);
        let j = (((z - self.oz) / self.cell).round().max(0.0) as usize).min(self.hh - 1);
        src[j * self.w + i]
    }
    pub fn height_at(&self, x: f64, z: f64) -> f64 {
        let (w, h) = (self.w as f64, self.hh as f64);
        let px = ((x - self.ox) / self.cell).clamp(0.0, w - 1.000001);
        let pz = ((z - self.oz) / self.cell).clamp(0.0, h - 1.000001);
        let (x0, z0) = (px as usize, pz as usize);
        let i0 = z0 * self.w + x0;
        let hv = &self.h;
        let (h00, h10, h01, h11) = (hv[i0] as f64, hv[i0 + 1] as f64, hv[i0 + self.w] as f64, hv[i0 + self.w + 1] as f64);
        let hm = 0.25 * (h00 + h10 + h01 + h11);
        let (fx, fz) = (px - x0 as f64 - 0.5, pz - z0 as f64 - 0.5);
        if fx.abs() >= fz.abs() {
            if fx >= 0.0 { hm + (h10 + h11 - 2.0 * hm) * fx + (h11 - h10) * fz } else { hm + (2.0 * hm - h00 - h01) * fx + (h01 - h00) * fz }
        } else if fz >= 0.0 {
            hm + (h11 - h01) * fx + (h01 + h11 - 2.0 * hm) * fz
        } else {
            hm + (h10 - h00) * fx + (2.0 * hm - h00 - h10) * fz
        }
    }
    pub fn inside(&self, x: f64, z: f64) -> bool {
        x >= self.ox && z >= self.oz && x <= self.x1 && z <= self.z1
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Pt {
    pub x: f64,
    pub z: f64,
}

/// Azimuth convention: 0 = +Z (map up), clockwise towards +X.
pub fn ray_dir(a: usize, n_az: usize) -> (f64, f64) {
    let t = 2.0 * std::f64::consts::PI * a as f64 / n_az as f64;
    (t.sin(), t.cos())
}

/// Sweep parameters (the page's params() plus the performance preset).
#[derive(Clone, Debug)]
pub struct SweepP {
    pub n_az: usize,
    pub ds: f64,
    pub range: f64,
    pub gun_h: f64,
    pub tgt_h: f64,
    pub y_pt: f64,
}

/// Reach from A: out[a*n+i] = 1 when some elevation puts the shell into [ground, ground+tgtH] at i*ds on ray a.
pub fn sweep_reach(t: &Terrain, tab: &Table, a_pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    let n = tab.n;
    let y_a = p.y_pt + p.gun_h;
    let mut out = vec![0u8; (a1 - a0) * n];
    let mut prof = vec![0f32; n];
    for a in a0..a1 {
        let (dx, dz) = ray_dir(a, p.n_az);
        let (mut n_max, mut top) = (n, f32::NEG_INFINITY);
        for (i, pr) in prof.iter_mut().enumerate() {
            let (x, z) = (a_pt.x + dx * i as f64 * tab.ds, a_pt.z + dz * i as f64 * tab.ds);
            if !t.inside(x, z) {
                n_max = i;
                break;
            }
            let g = (t.height_at(x, z) - y_a) as f32;
            *pr = g;
            top = top.max(g);
        }
        let ceil = top + p.tgt_h as f32;
        let row0 = (a - a0) * n;
        for k in 0..tab.ne {
            let (r, ap) = (k * n, tab.apex[k]);
            let mut i = 1;
            while i < n_max {
                let y = tab.y[r + i];
                if y.is_nan() {
                    break;
                }
                if y > ceil {
                    if i >= ap {
                        i += 1;
                        continue;
                    }
                    let (mut lo, mut hi) = (ap, (n_max - 1).min(tab.last[k]));
                    if hi <= lo || !(tab.y[r + hi] <= ceil) {
                        break;
                    }
                    while hi - lo > 1 {
                        let m = (lo + hi) >> 1;
                        if tab.y[r + m] <= ceil { hi = m } else { lo = m }
                    }
                    i = hi;
                    continue;
                }
                let rel = y - prof[i];
                if rel < 0.0 {
                    out[row0 + i] = 1;
                    break;
                }
                if rel <= p.tgt_h as f32 {
                    out[row0 + i] = 1;
                }
                i += 1;
            }
        }
    }
    out
}

/// Fractional elevation indices where row k passes height dy at column j.
fn solutions_at(tab: &Table, j: usize, dy: f32, res: &mut Vec<f32>) {
    res.clear();
    let n = tab.n;
    let mut pv = tab.y[j] - dy;
    for k in 1..tab.ne {
        let v = tab.y[k * n + j] - dy;
        if !pv.is_nan() && !v.is_nan() && (pv <= 0.0) != (v <= 0.0) {
            res.push((k - 1) as f32 + pv / (pv - v));
        }
        pv = v;
    }
}

/// Hit target B: bit 1 = lowest-arc solution from a shooter at j*ds on ray a clears terrain, bit 2 = a higher arc.
pub fn sweep_hit(t: &Terrain, tab: &Table, b_pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    let n = tab.n;
    let y_b = p.y_pt + p.tgt_h;
    let mut out = vec![0u8; (a1 - a0) * n];
    let mut g = vec![0f64; n];
    let mut sols = vec![];
    for a in a0..a1 {
        let (dx, dz) = ray_dir(a, p.n_az);
        let mut n_max = n;
        for (i, gi) in g.iter_mut().enumerate() {
            let (x, z) = (b_pt.x + dx * i as f64 * tab.ds, b_pt.z + dz * i as f64 * tab.ds);
            if !t.inside(x, z) {
                n_max = i;
                break;
            }
            *gi = t.height_at(x, z);
        }
        let row0 = (a - a0) * n;
        for j in 1..n_max {
            let y_p = g[j] + p.gun_h;
            solutions_at(tab, j, (y_b - y_p) as f32, &mut sols);
            for (si, &kf) in sols.iter().enumerate() {
                let k0 = (tab.ne - 2).min(kf as usize);
                let w = kf - k0 as f32;
                let (r0, r1) = (k0 * n, k0 * n + n);
                let clear = (1..j).all(|m| {
                    let y = y_p as f32 + tab.y[r0 + m] + (tab.y[r1 + m] - tab.y[r0 + m]) * w;
                    !(y < g[j - m] as f32)
                });
                if clear {
                    out[row0 + j] |= if si == 0 { 1 } else { 2 };
                }
            }
        }
    }
    out
}

/// Visibility from P: 0 hidden, 1 visible, 2 + src = the horizon point that cuts the view behind it.
pub fn sweep_view(t: &Terrain, pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    let n = (p.range / p.ds).ceil() as usize + 1;
    let y_e = p.y_pt + p.gun_h;
    let mut out = vec![0u8; (a1 - a0) * n];
    for a in a0..a1 {
        let (dx, dz) = ray_dir(a, p.n_az);
        let row0 = (a - a0) * n;
        let (mut max_s, mut hor, mut prev_vis) = (f64::NEG_INFINITY, 0usize, true);
        for i in 1..n {
            let d = i as f64 * p.ds;
            let (x, z) = (pt.x + dx * d, pt.z + dz * d);
            if !t.inside(x, z) {
                break;
            }
            let s = t.height_at(x, z);
            let top = if t.src_at(x, z) != 0 { s } else { s + p.tgt_h };
            let vis = (top - y_e) / d >= max_s;
            if vis {
                out[row0 + i] = 1;
            } else if prev_vis && hor > 0 {
                out[row0 + hor] = 2 + t.src_at(pt.x + dx * hor as f64 * p.ds, pt.z + dz * hor as f64 * p.ds);
            }
            prev_vis = vis;
            let sl = (s - y_e) / d;
            if sl > max_s {
                max_s = sl;
                hor = i;
            }
        }
    }
    out
}

/// Straight segment vs terrain: distance of the first blocking point.
pub fn los_blocked(t: &Terrain, a: Pt, y_a: f64, b: Pt, y_b: f64, step: f64) -> Option<f64> {
    let d = (b.x - a.x).hypot(b.z - a.z);
    if d < 1e-6 {
        return None;
    }
    let (dx, dz) = ((b.x - a.x) / d, (b.z - a.z) / d);
    let mut s = step;
    while s < d - 0.5 {
        if t.height_at(a.x + dx * s, a.z + dz * s) > y_a + (y_b - y_a) * s / d {
            return Some(s);
        }
        s += step;
    }
    None
}

/// Exact trajectory vs terrain along dir from a: distance of the first blocking point.
pub fn path_blocked(t: &Terrain, a: Pt, dir: (f64, f64), tr: &Traj, d: f64, step: f64) -> Option<f64> {
    let mut s = step;
    while s < d - 0.5 {
        let y = tr.at(Key::Y, s);
        if !(y >= t.height_at(a.x + dir.0 * s, a.z + dir.1 * s)) {
            return Some(s);
        }
        s += step;
    }
    None
}

#[cfg(test)]
mod tests {
    //! Ballistics, solver, terrain, sight line and the three sweeps on small synthetic maps.
    use super::*;

    fn shell(v0: f64, m: f64, cal: f64, cx: f64) -> Shell {
        Shell { kind: String::new(), type_title: None, v0, m, cal, cx, name: String::new(), en: None, cx_est: false, rocket: None }
    }
    fn l44() -> Shell { shell(1640.0, 4.3, 0.032, 0.30562) }
    fn vacuum() -> Shell { shell(1640.0, 4.3, 0.032, 0.0) }
    fn slow() -> Shell { shell(250.0, 20.0, 0.15, 0.3) }
    fn meta(w: usize, cell: f64) -> TerrainMeta { TerrainMeta { width: w, height: w, cell, ox: 0.0, oz: 0.0, hmin: -50.0, hmax: 200.0 } }
    fn q(h: f64) -> u16 { ((h + 50.0) / 250.0 * 65535.0).round() as u16 }
    fn flat(size: f64, cell: f64, f: impl Fn(f64, f64) -> f64) -> Terrain {
        let w = (size / cell).round() as usize + 1;
        let mut h16 = vec![0u16; w * w];
        for j in 0..w { for i in 0..w { h16[j * w + i] = q(f(i as f64 * cell, j as f64 * cell)); } }
        Terrain::new(&meta(w, cell), &h16, &[])
    }
    fn sp(n_az: usize, y_pt: f64) -> SweepP { SweepP { n_az, ds: 2.0, range: 150.0, gun_h: 2.0, tgt_h: 1.5, y_pt } }
    const P: fn(f64, f64) -> Pt = |x, z| Pt { x, z };

    #[test]
    fn roof_grid_raises_surface_only_where_set() {
        let w = 101;
        let h16 = vec![q(0.0); w * w];
        let mut g = Grid { base: 2.0, scale: 0.05, q: vec![0; w * w] };
        for j in 0..w { for i in 50..=52 { g.q[j * w + i] = 200; } }
        let t = Terrain::new(&meta(w, 2.0), &h16, &[]);
        let tc = Terrain::new(&meta(w, 2.0), &h16, &[&g]);
        assert!((tc.height_at(102.0, 50.0) - 12.0).abs() < 1e-3);
        assert_eq!(tc.src_at(102.0, 50.0), 1);
        assert_eq!(tc.src_at(20.0, 50.0), 0);
        assert!((tc.height_at(20.0, 50.0) - t.height_at(20.0, 50.0)).abs() < 1e-6);
        assert!(los_blocked(&tc, P(10.0, 50.0), 2.0, P(190.0, 50.0), 2.0, 1.0).is_some());
        assert!(los_blocked(&t, P(10.0, 50.0), 2.0, P(190.0, 50.0), 2.0, 1.0).is_none());
        let tab = build_table(&l44(), -5.0, 5.0, 0.5, 2.0, -50.0, 150.0, 2.0);
        let on_roof = sweep_reach(&tc, &tab, P(102.0, 50.0), &sp(4, tc.height_at(102.0, 50.0)), 0, 4);
        let on_ground = sweep_reach(&tc, &tab, P(102.0, 50.0), &sp(4, 0.0), 0, 4);
        let sum = |v: &[u8]| v.iter().map(|&b| b as u32).sum::<u32>();
        assert_ne!(sum(&on_roof), sum(&on_ground));
    }

    #[test]
    fn visibility_wall_and_pit() {
        let w = 201;
        let h16 = vec![q(0.0); w * w];
        let mut g = Grid { base: 2.0, scale: 0.05, q: vec![0; w * w] };
        for j in 0..w { g.q[j * w + 120] = 200; }
        let t = Terrain::new(&meta(w, 2.0), &h16, &[&g]);
        let out = sweep_view(&t, P(200.0, 200.0), &sp(4, 0.0), 0, 4);
        let n = 76;
        assert_eq!(out[30], 1);
        assert_eq!(out[n + 10], 1);
        assert_eq!(out[n + 20], 3);
        assert_eq!(out[n + 40], 0);
        let mut pit = vec![q(0.0); w * w];
        for j in 90..=110 { for i in 90..=110 { pit[j * w + i] = q(-10.0); } }
        let out2 = sweep_view(&Terrain::new(&meta(w, 2.0), &pit, &[]), P(200.0, 200.0), &sp(4, -10.0), 0, 4);
        assert_eq!(out2[50], 0);
        assert_eq!(out2[11], 2);
    }

    #[test]
    fn vacuum_matches_parabola() {
        let (sh, e) = (vacuum(), 5.0 * DEG);
        let tr = simulate(&sh, e, 0.0, 3000.0, SimOpt::default());
        for s in [500.0, 1500.0, 2900.0] {
            let exact = s * e.tan() - G * s * s / (2.0 * sh.v0.powi(2) * e.cos().powi(2));
            assert!((tr.at(Key::Y, s) - exact).abs() < 0.01, "s={s}");
        }
    }

    #[test]
    fn drag_slows_and_drops() {
        let a = simulate(&l44(), 0.0, 0.0, 2000.0, SimOpt::default());
        let b = simulate(&vacuum(), 0.0, 0.0, 2000.0, SimOpt::default());
        assert!(a.at(Key::V, 2000.0) < 1640.0 - 50.0);
        assert!(a.at(Key::Y, 2000.0) < b.at(Key::Y, 2000.0));
        assert!(a.at(Key::T, 2000.0) > b.at(Key::T, 2000.0));
    }

    #[test]
    fn solve_exact_low_arc_first() {
        let sols = solve_exact(&l44(), 1800.0, 12.0, 100.0, -10.0, 20.0);
        assert!(!sols.is_empty());
        let tr = simulate(&l44(), sols[0], 100.0, 1801.0, SimOpt::default());
        assert!((tr.at(Key::Y, 1800.0) - 112.0).abs() < 0.02);
        let two = solve_exact(&slow(), 2000.0, 0.0, 0.0, 0.0, 80.0);
        assert_eq!(two.len(), 2);
        assert!(two[0] < two[1]);
    }

    #[test]
    fn terrain_interpolation() {
        let t = flat(100.0, 2.0, |x, z| 0.1 * x + 0.05 * z);
        assert!((t.height_at(10.0, 20.0) - 2.0).abs() < 0.01);
        assert!((t.height_at(11.3, 20.7) - (1.13 + 1.035)).abs() < 0.01);
    }

    #[test]
    fn sight_line_wall() {
        let fl = flat(400.0, 2.0, |_, _| 0.0);
        let wall = flat(400.0, 2.0, |x, _| if x > 198.0 && x < 204.0 { 10.0 } else { 0.0 });
        let (a, b) = (P(50.0, 100.0), P(350.0, 100.0));
        assert!(los_blocked(&fl, a, 2.0, b, 1.5, 1.0).is_none());
        let at = los_blocked(&wall, a, 2.0, b, 1.5, 1.0).unwrap();
        assert!(at > 145.0 && at < 156.0, "blocked at {at}");
    }

    #[test]
    fn table_matches_simulation() {
        let tab = build_table(&l44(), -2.0, 4.0, 0.5, 0.0, -500.0, 2000.0, 4.0);
        let tr = simulate(&l44(), tab.elev[6], 0.0, 2001.0, SimOpt::default());
        for i in [100, 300, 500] {
            assert!((tab.y[6 * tab.n + i] as f64 - tr.at(Key::Y, i as f64 * 4.0)).abs() < 0.05);
        }
    }

    #[test]
    fn reach_flat_and_ridge() {
        let p = SweepP { n_az: 8, ds: 4.0, range: 1200.0, gun_h: 2.0, tgt_h: 1.5, y_pt: 0.0 };
        let a = P(1500.0, 1500.0);
        let tab = build_table(&l44(), -10.0, 20.0, 0.05, 2.0, -60.0, 1200.0, 4.0);
        let out = sweep_reach(&flat(3000.0, 4.0, |_, _| 0.0), &tab, a, &p, 0, 8);
        for i in [10, 100, 250] { assert_eq!(out[i], 1, "ray 0 at {} m", i * 4); }
        let ridge = flat(3000.0, 4.0, |_, z| if z > 2300.0 && z < 2320.0 { 60.0 } else { 0.0 });
        let tab = build_table(&l44(), -10.0, 3.0, 0.05, 2.0, -60.0, 1200.0, 4.0);
        let out = sweep_reach(&ridge, &tab, a, &p, 0, 8);
        assert_eq!(out[150], 1);
        assert_eq!(out[260], 0);
    }

    #[test]
    fn hit_high_arc_behind_ridge() {
        let ridge = flat(4000.0, 4.0, |_, z| if z > 1800.0 && z < 1830.0 { 40.0 } else { 0.0 });
        let p = SweepP { n_az: 8, ds: 4.0, range: 1000.0, gun_h: 2.0, tgt_h: 1.5, y_pt: 0.0 };
        let tab = build_table(&slow(), 0.0, 89.0, 0.1, 1.5, -60.0, 1000.0, 4.0);
        let out = sweep_hit(&ridge, &tab, P(2000.0, 1500.0), &p, 0, 1);
        assert_eq!(out[37] & 1, 1);
        assert_eq!(out[200] & 1, 0);
        assert_eq!(out[200] & 2, 2);
    }

    #[test]
    fn rocket_motor_bm_31_12_solves_arcs() {
        let m31 = Shell {
            kind: "rocket_tank".into(),
            type_title: None,
            v0: 245.0,
            m: 91.5,
            cal: 0.3,
            cx: 0.825,
            name: "300mm M-31UK".into(),
            en: None,
            cx_est: false,
            rocket: Some(RocketMotor { v_start: 0.0, force0: 20700.0, t_fire0: 1.0, m_end0: 80.25, ..Default::default() }),
        };
        let tr45 = simulate(&m31, 45.0 * DEG, 0.0, 6000.0, SimOpt { y_floor: Some(-1.0), ..Default::default() });
        let peak_v = tr45.v.iter().copied().fold(0.0f64, f64::max);
        assert!(peak_v > 225.0 && peak_v < 255.0, "peak_v={peak_v}");
        let max_range = *tr45.s.last().unwrap();
        assert!(max_range > 2100.0 && max_range < 2600.0, "max_range={max_range}");
        let sols = solve_exact(&m31, 1800.0, 0.0, 0.0, 10.0, 48.0);
        assert_eq!(sols.len(), 1, "BM-31-12 low arc hits 1800 m within 10..48 deg");
        let sols_high = solve_exact(&m31, 1800.0, 0.0, 0.0, 10.0, 80.0);
        assert_eq!(sols_high.len(), 2, "both low and high arcs exist up to 80 deg");
    }
}
