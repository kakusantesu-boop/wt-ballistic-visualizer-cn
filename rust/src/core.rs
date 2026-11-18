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
    #[serde(rename = "tLife", default)]
    pub t_life: f64,
    #[serde(rename = "maxDist", default)]
    pub max_dist: f64,
    #[serde(default)]
    pub disp: f64,
    #[serde(rename = "cxMach", default)]
    pub cx_mach: bool,
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
    #[serde(rename = "maxDist", default)]
    pub max_dist: f64,
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
    let u = 2.25577e-5 * y;
    if u >= 1.0 {
        return 0.0;
    }
    if u > -0.25 && u < 0.35 {
        let f = 1.0 - u;
        let f2 = f * f;
        return 1.225 * (f2 * f2) * (1.0 - u * (0.2559 + u * (0.095208 + u * 0.05535)));
    }
    1.225 * (1.0 - u).powf(4.2559)
}
pub fn sound_speed(y: f64) -> f64 {
    let f = (1.0 - 2.25577e-5 * y.max(0.0)).max(0.5);
    340.29 * f.sqrt()
}
pub fn rocket_cd0(mach: f64) -> f64 {
    if mach <= 0.8 {
        0.29
    } else if mach <= 1.1 {
        let u = (mach - 0.8) / 0.3;
        0.29 + 0.39 * (u * u * (3.0 - 2.0 * u))
    } else {
        0.26 + 0.42 / (1.0 + 0.55 * (mach - 1.1))
    }
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
    pub self_destruct: Option<&'static str>,
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

#[derive(Clone, Copy)]
pub struct SimOpt {
    pub dt: f64,
    pub max_t: f64,
    pub y_floor: Option<f64>,
    pub v_air: f64,
}
impl Default for SimOpt {
    fn default() -> Self {
        Self { dt: 0.004, max_t: 60.0, y_floor: None, v_air: 0.0 }
    }
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct RocketEnvelope {
    pub max_s: f64,
    pub elev_deg: f64,
    pub tof: f64,
    pub reason: &'static str,
    pub t_life: f64,
    pub max_dist: f64,
}

/// Point-mass flight in the vertical plane of the shot. Heun (RK2) steps; supports both conventional shells and two-stage rockets.
pub fn simulate(sh: &Shell, elev: f64, y0: f64, max_s: f64, opt: SimOpt) -> Traj {
    let dt = opt.dt;
    let v_air = opt.v_air;
    let y_floor = opt.y_floor.unwrap_or(y0 - 3000.0);
    if let Some(rm) = &sh.rocket {
        if rm.force0 > 0.0 && rm.t_fire0 > 0.0 {
            let (dir_x, dir_y) = (elev.cos(), elev.sin());
            let area = std::f64::consts::PI * 0.25 * sh.cal * sh.cal;
            let t0_start = rm.t_delay.max(0.0);
            let t0_end = t0_start + rm.t_fire0;
            let t1_end = t0_end + rm.t_fire1.max(0.0);
            let m0_end = if rm.m_end0 > 0.0 { rm.m_end0 } else { sh.m };
            let m1_end = if rm.t_fire1 > 0.0 && rm.m_end1 > 0.0 { rm.m_end1 } else { m0_end };
            let t_life = if rm.t_life > 0.0 { rm.t_life } else { 0.0 };
            let max_d = if rm.max_dist > 0.0 { rm.max_dist } else if sh.max_dist > 0.0 { sh.max_dist } else { 0.0 };
            let use_mach = rm.cx_mach;
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
            let accel_free = |yc: f64, vxc: f64, vyc: f64, th: f64, tt: f64| -> (f64, f64) {
                let (mass, thrust) = stage_at(tt);
                let spd = vxc.hypot(vyc);
                let (ux, uy) = if spd > 1e-3 { (vxc / spd, vyc / spd) } else { (th.cos(), th.sin()) };
                let cd = if use_mach { sh.cx * rocket_cd0(spd / sound_speed(yc)) } else { sh.cx };
                let a_drag = (0.5 * cd * area * rho(yc) * spd * spd) / mass.max(1e-3);
                let a_thrust = thrust / mass.max(1e-3);
                (a_thrust * th.cos() - a_drag * ux, a_thrust * th.sin() - a_drag * uy - G)
            };
            let v_init = rm.v_start.max(0.0);
            let (mut s, mut y, mut t, mut theta, mut u_tube) = (0.0f64, y0, 0.0f64, elev, v_init);
            let (mut vx, mut vy) = (v_air + v_init * dir_x, v_init * dir_y);
            let mut on_rail = v_init < 5.0;
            let mut rail_dist = 0.0;
            let mut self_destruct = None;
            let mut tr = Traj { s: vec![0.0], y: vec![y0], t: vec![0.0], v: vec![vx.hypot(vy)], self_destruct: None };
            while s < max_s && t < opt.max_t && y > y_floor {
                if t_life > 0.0 && t >= t_life {
                    self_destruct = Some("time");
                    break;
                }
                if max_d > 0.0 && s.hypot(y - y0) >= max_d {
                    self_destruct = Some("dist");
                    break;
                }
                if on_rail {
                    let (mass, thrust) = stage_at(t);
                    if thrust > 0.0 && rail_dist < 2.0 {
                        let spd = vx.hypot(vy);
                        let cd = if use_mach { sh.cx * rocket_cd0(spd / sound_speed(y)) } else { sh.cx };
                        let f_drag = 0.5 * cd * area * rho(y) * spd * spd;
                        let a_tube = ((thrust - f_drag) / mass.max(1e-3) - G * dir_y).max(0.0);
                        let u_next = u_tube + a_tube * dt;
                        let u_mid = 0.5 * (u_tube + u_next);
                        rail_dist += u_mid * dt;
                        u_tube = u_next;
                        s += (v_air + u_mid * dir_x) * dt;
                        y += (u_mid * dir_y) * dt;
                        vx = v_air + u_tube * dir_x;
                        vy = u_tube * dir_y;
                        t += dt;
                        tr.s.push(s);
                        tr.y.push(y);
                        tr.t.push(t);
                        tr.v.push(vx.hypot(vy));
                        continue;
                    }
                    on_rail = false;
                }
                let spd_now = vx.hypot(vy);
                let th_next = if spd_now > 0.5 { theta + (vy.atan2(vx) - theta) * (0.0025 * spd_now * dt).min(1.0) } else { theta };
                let (ax1, ay1) = accel_free(y, vx, vy, theta, t);
                let (vx2, vy2) = (vx + ax1 * dt, vy + ay1 * dt);
                let (ax2, ay2) = accel_free(y + vy * dt, vx2, vy2, th_next, t + dt);
                s += 0.5 * (vx + vx2) * dt;
                y += 0.5 * (vy + vy2) * dt;
                vx += 0.5 * (ax1 + ax2) * dt;
                vy += 0.5 * (ay1 + ay2) * dt;
                theta = th_next;
                t += dt;
                tr.s.push(s);
                tr.y.push(y);
                tr.t.push(t);
                tr.v.push(vx.hypot(vy));
            }
            tr.self_destruct = self_destruct;
            return tr;
        }
    }
    let k = drag_k(sh);
    let max_d = if sh.max_dist > 0.0 { sh.max_dist } else { 0.0 };
    let (mut s, mut y, mut t) = (0.0f64, y0, 0.0f64);
    let (mut vx, mut vy) = (v_air + sh.v0 * elev.cos(), sh.v0 * elev.sin());
    let mut self_destruct = None;
    let mut tr = Traj { s: vec![0.0], y: vec![y0], t: vec![0.0], v: vec![vx.hypot(vy)], self_destruct: None };
    while s < max_s && t < opt.max_t && y > y_floor {
        if max_d > 0.0 && s.hypot(y - y0) >= max_d {
            self_destruct = Some("dist");
            break;
        }
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
    tr.self_destruct = self_destruct;
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

/// Zero-allocation point-mass flight to horizontal distance `d`: returns `(y_at_d, t_at_d)`, or `(NAN, NAN)` if unreachable / self-destructed.
pub fn sim_at_dist(sh: &Shell, elev: f64, y0: f64, d: f64, y_floor: f64, v_air: f64) -> (f64, f64) {
    if d <= 0.0 {
        return (y0, 0.0);
    }
    let dt = 0.004;
    let max_t = 60.0;
    if let Some(rm) = &sh.rocket {
        if rm.force0 > 0.0 && rm.t_fire0 > 0.0 {
            let (dir_x, dir_y) = (elev.cos(), elev.sin());
            let area = std::f64::consts::PI * 0.25 * sh.cal * sh.cal;
            let t0_start = rm.t_delay.max(0.0);
            let t0_end = t0_start + rm.t_fire0;
            let t1_end = t0_end + rm.t_fire1.max(0.0);
            let m0_end = if rm.m_end0 > 0.0 { rm.m_end0 } else { sh.m };
            let m1_end = if rm.t_fire1 > 0.0 && rm.m_end1 > 0.0 { rm.m_end1 } else { m0_end };
            let t_life = if rm.t_life > 0.0 { rm.t_life } else { 0.0 };
            let max_d = if rm.max_dist > 0.0 { rm.max_dist } else if sh.max_dist > 0.0 { sh.max_dist } else { 0.0 };
            let use_mach = rm.cx_mach;
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
            let accel_free = |yc: f64, vxc: f64, vyc: f64, th: f64, tt: f64| -> (f64, f64) {
                let (mass, thrust) = stage_at(tt);
                let spd = vxc.hypot(vyc);
                let (ux, uy) = if spd > 1e-3 { (vxc / spd, vyc / spd) } else { (th.cos(), th.sin()) };
                let cd = if use_mach { sh.cx * rocket_cd0(spd / sound_speed(yc)) } else { sh.cx };
                let a_drag = (0.5 * cd * area * rho(yc) * spd * spd) / mass.max(1e-3);
                let a_thrust = thrust / mass.max(1e-3);
                (a_thrust * th.cos() - a_drag * ux, a_thrust * th.sin() - a_drag * uy - G)
            };
            let v_init = rm.v_start.max(0.0);
            let (mut s, mut y, mut t, mut theta, mut u_tube) = (0.0f64, y0, 0.0f64, elev, v_init);
            let (mut vx, mut vy) = (v_air + v_init * dir_x, v_init * dir_y);
            let mut on_rail = v_init < 5.0;
            let mut rail_dist = 0.0;
            while t < max_t && y > y_floor {
                if t_life > 0.0 && t >= t_life {
                    return (f64::NAN, f64::NAN);
                }
                if max_d > 0.0 && s.hypot(y - y0) >= max_d {
                    return (f64::NAN, f64::NAN);
                }
                let (sp, yp, tp) = (s, y, t);
                if on_rail {
                    let (mass, thrust) = stage_at(t);
                    if thrust > 0.0 && rail_dist < 2.0 {
                        let spd = vx.hypot(vy);
                        let cd = if use_mach { sh.cx * rocket_cd0(spd / sound_speed(y)) } else { sh.cx };
                        let f_drag = 0.5 * cd * area * rho(y) * spd * spd;
                        let a_tube = ((thrust - f_drag) / mass.max(1e-3) - G * dir_y).max(0.0);
                        let u_next = u_tube + a_tube * dt;
                        let u_mid = 0.5 * (u_tube + u_next);
                        rail_dist += u_mid * dt;
                        u_tube = u_next;
                        s += (v_air + u_mid * dir_x) * dt;
                        y += (u_mid * dir_y) * dt;
                        vx = v_air + u_tube * dir_x;
                        vy = u_tube * dir_y;
                        t += dt;
                        if s >= d {
                            let w = if s > sp { (d - sp) / (s - sp) } else { 0.0 };
                            return (yp + (y - yp) * w, tp + (t - tp) * w);
                        }
                        continue;
                    }
                    on_rail = false;
                }
                let spd_now = vx.hypot(vy);
                let th_next = if spd_now > 0.5 { theta + (vy.atan2(vx) - theta) * (0.0025 * spd_now * dt).min(1.0) } else { theta };
                let (ax1, ay1) = accel_free(y, vx, vy, theta, t);
                let (vx2, vy2) = (vx + ax1 * dt, vy + ay1 * dt);
                let (ax2, ay2) = accel_free(y + vy * dt, vx2, vy2, th_next, t + dt);
                s += 0.5 * (vx + vx2) * dt;
                y += 0.5 * (vy + vy2) * dt;
                vx += 0.5 * (ax1 + ax2) * dt;
                vy += 0.5 * (ay1 + ay2) * dt;
                theta = th_next;
                t += dt;
                if s >= d {
                    let w = if s > sp { (d - sp) / (s - sp) } else { 0.0 };
                    let yd = yp + (y - yp) * w;
                    let td = tp + (t - tp) * w;
                    if (t_life > 0.0 && td > t_life) || (max_d > 0.0 && d.hypot(yd - y0) > max_d) {
                        return (f64::NAN, f64::NAN);
                    }
                    return (yd, td);
                }
                if vx < -1.0 && s < sp {
                    return (f64::NAN, f64::NAN);
                }
            }
            return (f64::NAN, f64::NAN);
        }
    }
    let k = drag_k(sh);
    let max_d = if sh.max_dist > 0.0 { sh.max_dist } else { 0.0 };
    let (mut s, mut y, mut t) = (0.0f64, y0, 0.0f64);
    let (mut vx, mut vy) = (v_air + sh.v0 * elev.cos(), sh.v0 * elev.sin());
    if vx <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    while t < max_t && y > y_floor {
        if max_d > 0.0 && s.hypot(y - y0) >= max_d {
            return (f64::NAN, f64::NAN);
        }
        let (sp, yp, tp) = (s, y, t);
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
        if s >= d {
            let w = if s > sp { (d - sp) / (s - sp) } else { 0.0 };
            let yd = yp + (y - yp) * w;
            let td = tp + (t - tp) * w;
            if max_d > 0.0 && d.hypot(yd - y0) > max_d {
                return (f64::NAN, f64::NAN);
            }
            return (yd, td);
        }
    }
    (f64::NAN, f64::NAN)
}

/// Elevations (rad, within pitch limits in deg) whose path passes through (d, y0 + dy). Low arc first.
#[cfg_attr(not(test), allow(dead_code))]
pub fn solve_exact(sh: &Shell, d: f64, dy: f64, y0: f64, p_min: f64, p_max: f64) -> Vec<f64> {
    solve_exact_air(sh, d, dy, y0, p_min, p_max, 0.0)
}

pub fn solve_exact_air(sh: &Shell, d: f64, dy: f64, y0: f64, p_min: f64, p_max: f64, v_air: f64) -> Vec<f64> {
    if d <= 0.5 {
        return vec![];
    }
    let y_floor = y0 + dy.min(0.0) - 260.0;
    let fan = build_traj_fan(sh, y0, p_min, p_max, v_air, d + 40.0, y_floor);
    let coarse = fan.solve(sh, d, dy);
    let f = |e: f64| sim_at_dist(sh, e, y0, d, y_floor - 200.0, v_air).0 - y0 - dy;
    let mut sols = Vec::with_capacity(coarse.len());
    let margin = 0.6 * DEG;
    for (e_est, _) in coarse {
        let lo0 = (p_min * DEG).max(e_est - margin);
        let hi0 = (p_max * DEG).min(e_est + margin);
        let (mut pe, mut pv) = (lo0, f(lo0));
        let mut refined = None;
        for step in 1..=6 {
            let e = lo0 + (hi0 - lo0) * step as f64 / 6.0;
            let v = f(e);
            if pv.is_finite() && v.is_finite() && (pv <= 0.0) != (v <= 0.0) {
                let (mut lo, mut hi, mut flo) = (pe, e, pv);
                for _ in 0..20 {
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
                        hi = mid;
                    }
                }
                let root = 0.5 * (lo + hi);
                if f(root).is_finite() {
                    refined = Some(root);
                    break;
                }
            }
            pe = e;
            pv = v;
        }
        if let Some(r) = refined {
            if sols.last().is_none_or(|&prev: &f64| (r - prev).abs() > 1e-3) {
                sols.push(r);
            }
        } else if f(e_est).is_finite() && f(e_est).abs() < 2.5 {
            sols.push(e_est);
        }
    }
    sols
}

/// Pre-computed elevation fan for solving dozens of targets / range-ladder steps and the max-range envelope in < 1.5 ms total.
pub struct TrajFan {
    pub y0: f64,
    pub v_air: f64,
    pub ds: f64,
    pub n: usize,
    pub ne: usize,
    pub elev: Vec<f64>,
    pub y: Vec<f32>,
    pub t: Vec<f32>,
    pub last: Vec<usize>,
    end_s: Vec<f64>,
    end_y: Vec<f64>,
    end_t: Vec<f64>,
    sd: Vec<Option<&'static str>>,
    t_life: f64,
    max_d: f64,
}

pub fn build_traj_fan(sh: &Shell, y0: f64, p_min: f64, p_max: f64, v_air: f64, max_s: f64, y_floor: f64) -> TrajFan {
    let step_deg = 0.5;
    let ne = 2.max(((p_max - p_min) / step_deg).ceil() as usize + 1);
    let ds = 20.0;
    let n = (max_s / ds).ceil() as usize + 2;
    let mut y = vec![f32::NAN; ne * n];
    let mut t_grid = vec![f32::NAN; ne * n];
    let mut elev = Vec::with_capacity(ne);
    let mut last = vec![0usize; ne];
    let mut end_s = vec![0.0; ne];
    let mut end_y = vec![y0; ne];
    let mut end_t = vec![0.0; ne];
    let mut sd = vec![None; ne];
    let has_motor = sh.rocket.as_ref().is_some_and(|rm| rm.force0 > 0.0 && rm.t_fire0 > 0.0);
    let rm = sh.rocket.unwrap_or_default();
    let t_life = if has_motor && rm.t_life > 0.0 { rm.t_life } else { 0.0 };
    let max_d = if has_motor && rm.max_dist > 0.0 { rm.max_dist } else { sh.max_dist.max(0.0) };
    let area = std::f64::consts::PI * 0.25 * sh.cal * sh.cal;
    let t0_start = if has_motor { rm.t_delay.max(0.0) } else { 0.0 };
    let t0_end = if has_motor { t0_start + rm.t_fire0 } else { 0.0 };
    let t1_fire = if has_motor { rm.t_fire1.max(0.0) } else { 0.0 };
    let t1_end = t0_end + t1_fire;
    let m0_end = if has_motor && rm.m_end0 > 0.0 { rm.m_end0 } else { sh.m };
    let m1_end = if has_motor && t1_fire > 0.0 && rm.m_end1 > 0.0 { rm.m_end1 } else { m0_end };
    let use_mach = has_motor && rm.cx_mach;
    let v_start = if has_motor { rm.v_start.max(0.0) } else { sh.v0 };
    let k_shell = if has_motor { 0.0 } else { drag_k(sh) };

    for k in 0..ne {
        let deg = (p_min + (p_max - p_min) * k as f64 / (ne - 1) as f64).min(p_max);
        let e = deg * DEG;
        elev.push(e);
        let (dir_x, dir_y) = (e.cos(), e.sin());
        let row = k * n;
        y[row] = 0.0;
        t_grid[row] = 0.0;
        last[k] = 0;
        let (mut next_i, mut next_s) = (1usize, ds);
        let (mut s, mut yc, mut t) = (0.0f64, y0, 0.0f64);
        let mut self_destruct = None;

        if has_motor {
            let (mut theta, mut u_tube) = (e, v_start);
            let (mut vx, mut vy) = (v_air + v_start * dir_x, v_start * dir_y);
            let mut on_rail = v_start < 5.0;
            let mut rail_dist = 0.0f64;
            while s < max_s + ds && t < 60.0 && yc > y_floor {
                if t_life > 0.0 && t >= t_life {
                    self_destruct = Some("time");
                    break;
                }
                if max_d > 0.0 && s * s + (yc - y0) * (yc - y0) >= max_d * max_d {
                    self_destruct = Some("dist");
                    break;
                }
                let (sp, yp, tp) = (s, yc, t);
                let dt = if t <= t1_end + 0.02 { 0.004 } else { 0.016 };
                if on_rail {
                    let mass = if t < t0_start { sh.m } else if t < t0_end { sh.m + (m0_end - sh.m) * ((t - t0_start) / rm.t_fire0) } else { m0_end };
                    let thrust = if t < t0_start { 0.0 } else if t < t0_end { rm.force0 } else if t1_fire > 0.0 && t < t1_end { rm.force1 } else { 0.0 };
                    if thrust > 0.0 && rail_dist < 2.0 {
                        let spd = (vx * vx + vy * vy).sqrt();
                        let cd = if use_mach { sh.cx * rocket_cd0(spd / sound_speed(yc)) } else { sh.cx };
                        let f_drag = 0.5 * cd * area * rho(yc) * spd * spd;
                        let a_tube = ((thrust - f_drag) / mass.max(1e-3) - G * dir_y).max(0.0);
                        let u_next = u_tube + a_tube * dt;
                        let u_mid = 0.5 * (u_tube + u_next);
                        rail_dist += u_mid * dt;
                        u_tube = u_next;
                        s += (v_air + u_mid * dir_x) * dt;
                        yc += (u_mid * dir_y) * dt;
                        vx = v_air + u_tube * dir_x;
                        vy = u_tube * dir_y;
                        t += dt;
                        while next_i < n && s >= next_s {
                            let w = if s > sp { (next_s - sp) / (s - sp) } else { 0.0 };
                            y[row + next_i] = (yp + (yc - yp) * w - y0) as f32;
                            t_grid[row + next_i] = (tp + (t - tp) * w) as f32;
                            last[k] = next_i;
                            next_i += 1;
                            next_s = next_i as f64 * ds;
                        }
                        continue;
                    }
                    on_rail = false;
                }
                if t < t1_end + 0.02 {
                    let spd_now = (vx * vx + vy * vy).sqrt();
                    let th_next = if spd_now > 0.5 { theta + (vy.atan2(vx) - theta) * (0.0025 * spd_now * dt).min(1.0) } else { theta };
                    let (m1, thr1) = if t < t0_start { (sh.m, 0.0) } else if t < t0_end { (sh.m + (m0_end - sh.m) * ((t - t0_start) / rm.t_fire0), rm.force0) } else if t1_fire > 0.0 && t < t1_end { (m0_end + (m1_end - m0_end) * ((t - t0_end) / t1_fire), rm.force1) } else { (m1_end, 0.0) };
                    let cd1 = if use_mach { sh.cx * rocket_cd0(spd_now / sound_speed(yc)) } else { sh.cx };
                    let ad1 = (0.5 * cd1 * area * rho(yc) * spd_now) / m1.max(1e-3);
                    let ax1 = (thr1 / m1.max(1e-3)) * theta.cos() - ad1 * vx;
                    let ay1 = (thr1 / m1.max(1e-3)) * theta.sin() - ad1 * vy - G;

                    let (vx2, vy2, y2, t2) = (vx + ax1 * dt, vy + ay1 * dt, yc + vy * dt, t + dt);
                    let spd2 = (vx2 * vx2 + vy2 * vy2).sqrt();
                    let (m2, thr2) = if t2 < t0_start { (sh.m, 0.0) } else if t2 < t0_end { (sh.m + (m0_end - sh.m) * ((t2 - t0_start) / rm.t_fire0), rm.force0) } else if t1_fire > 0.0 && t2 < t1_end { (m0_end + (m1_end - m0_end) * ((t2 - t0_end) / t1_fire), rm.force1) } else { (m1_end, 0.0) };
                    let cd2 = if use_mach { sh.cx * rocket_cd0(spd2 / sound_speed(y2)) } else { sh.cx };
                    let ad2 = (0.5 * cd2 * area * rho(y2) * spd2) / m2.max(1e-3);
                    let ax2 = (thr2 / m2.max(1e-3)) * th_next.cos() - ad2 * vx2;
                    let ay2 = (thr2 / m2.max(1e-3)) * th_next.sin() - ad2 * vy2 - G;

                    s += 0.5 * (vx + vx2) * dt;
                    yc += 0.5 * (vy + vy2) * dt;
                    vx += 0.5 * (ax1 + ax2) * dt;
                    vy += 0.5 * (ay1 + ay2) * dt;
                    theta = th_next;
                    t = t2;
                } else {
                    let k_coast = (0.5 * area) / m1_end.max(1e-3);
                    let spd1 = (vx * vx + vy * vy).sqrt();
                    let cd1 = if use_mach { sh.cx * rocket_cd0(spd1 / sound_speed(yc)) } else { sh.cx };
                    let c1 = k_coast * cd1 * rho(yc) * spd1;
                    let (ax1, ay1) = (-c1 * vx, -c1 * vy - G);
                    let (vx2, vy2, y2) = (vx + ax1 * dt, vy + ay1 * dt, yc + vy * dt);
                    let spd2 = (vx2 * vx2 + vy2 * vy2).sqrt();
                    let cd2 = if use_mach { sh.cx * rocket_cd0(spd2 / sound_speed(y2)) } else { sh.cx };
                    let c2 = k_coast * cd2 * rho(y2) * spd2;
                    let (ax2, ay2) = (-c2 * vx2, -c2 * vy2 - G);
                    s += 0.5 * (vx + vx2) * dt;
                    yc += 0.5 * (vy + vy2) * dt;
                    vx += 0.5 * (ax1 + ax2) * dt;
                    vy += 0.5 * (ay1 + ay2) * dt;
                    t += dt;
                }
                while next_i < n && s >= next_s {
                    let w = if s > sp { (next_s - sp) / (s - sp) } else { 0.0 };
                    let yd = yp + (yc - yp) * w;
                    let td = tp + (t - tp) * w;
                    if (t_life > 0.0 && td > t_life) || (max_d > 0.0 && next_s * next_s + (yd - y0) * (yd - y0) > max_d * max_d) {
                        self_destruct = Some(if t_life > 0.0 && td > t_life { "time" } else { "dist" });
                        break;
                    }
                    y[row + next_i] = (yd - y0) as f32;
                    t_grid[row + next_i] = td as f32;
                    last[k] = next_i;
                    next_i += 1;
                    next_s = next_i as f64 * ds;
                }
                if self_destruct.is_some() || (vx < -1.0 && s < sp) {
                    break;
                }
            }
        } else {
            let (mut vx, mut vy) = (v_air + v_start * dir_x, v_start * dir_y);
            let dt = 0.012;
            if vx > 0.0 {
                while s < max_s + ds && t < 60.0 && yc > y_floor {
                    if max_d > 0.0 && s * s + (yc - y0) * (yc - y0) >= max_d * max_d {
                        self_destruct = Some("dist");
                        break;
                    }
                    let (sp, yp, tp) = (s, yc, t);
                    let c1 = k_shell * rho(yc) * (vx * vx + vy * vy).sqrt();
                    let (ax1, ay1) = (-c1 * vx, -G - c1 * vy);
                    let (vx2, vy2) = (vx + ax1 * dt, vy + ay1 * dt);
                    let c2 = k_shell * rho(yc + vy * dt) * (vx2 * vx2 + vy2 * vy2).sqrt();
                    let (ax2, ay2) = (-c2 * vx2, -G - c2 * vy2);
                    s += 0.5 * (vx + vx2) * dt;
                    yc += 0.5 * (vy + vy2) * dt;
                    vx += 0.5 * (ax1 + ax2) * dt;
                    vy += 0.5 * (ay1 + ay2) * dt;
                    t += dt;
                    while next_i < n && s >= next_s {
                        let w = if s > sp { (next_s - sp) / (s - sp) } else { 0.0 };
                        y[row + next_i] = (yp + (yc - yp) * w - y0) as f32;
                        t_grid[row + next_i] = (tp + (t - tp) * w) as f32;
                        last[k] = next_i;
                        next_i += 1;
                        next_s = next_i as f64 * ds;
                    }
                }
            }
        }
        end_s[k] = s;
        end_y[k] = yc;
        end_t[k] = t;
        sd[k] = self_destruct;
    }
    TrajFan { y0, v_air, ds, n, ne, elev, y, t: t_grid, last, end_s, end_y, end_t, sd, t_life, max_d }
}

impl TrajFan {
    fn interp_at(&self, k: usize, d: f64) -> (f64, f64) {
        if d < 0.0 {
            return (f64::NAN, f64::NAN);
        }
        let fi = d / self.ds;
        let i0 = fi.floor() as usize;
        let row = k * self.n;
        if i0 < self.last[k] {
            let w = fi - i0 as f64;
            let y0 = self.y[row + i0] as f64;
            let y1 = self.y[row + i0 + 1] as f64;
            let t0 = self.t[row + i0] as f64;
            let t1 = self.t[row + i0 + 1] as f64;
            return (y0 + (y1 - y0) * w, t0 + (t1 - t0) * w);
        }
        let s0 = self.last[k] as f64 * self.ds;
        if self.sd[k].is_none() && d <= self.end_s[k] && self.end_s[k] > s0 {
            let w = (d - s0) / (self.end_s[k] - s0);
            let y0 = self.y[row + self.last[k]] as f64;
            let t0 = self.t[row + self.last[k]] as f64;
            return (y0 + ((self.end_y[k] - self.y0) - y0) * w, t0 + (self.end_t[k] - t0) * w);
        }
        if (d - s0).abs() < 1e-6 {
            return (self.y[row + self.last[k]] as f64, self.t[row + self.last[k]] as f64);
        }
        (f64::NAN, f64::NAN)
    }

    /// Solve all valid elevations `(elev_rad, tof_s)` passing through `(d, y0 + dy)`, low arc first.
    pub fn solve(&self, _sh: &Shell, d: f64, dy: f64) -> Vec<(f64, f64)> {
        if d <= 0.5 {
            return vec![];
        }
        let _ = self.v_air;
        let mut sols = vec![];
        let (py, mut pt) = self.interp_at(0, d);
        let mut pv = py - dy;
        for k in 1..self.ne {
            let (cy, ct) = self.interp_at(k, d);
            let v = cy - dy;
            if pv.is_finite() && v.is_finite() && (pv <= 0.0) != (v <= 0.0) {
                let w = pv / (pv - v);
                sols.push((self.elev[k - 1] + (self.elev[k] - self.elev[k - 1]) * w, pt + (ct - pt) * w));
            }
            pv = v;
            pt = ct;
        }
        sols
    }

    fn reach_row(&self, k: usize, y_target: f64) -> (f64, f64) {
        let dy_t = (y_target - self.y0) as f32;
        let row = k * self.n;
        for i in 1..=self.last[k] {
            let y_prev = self.y[row + i - 1];
            let y_cur = self.y[row + i];
            if y_cur <= dy_t && y_prev >= dy_t {
                let w = ((y_prev - dy_t) / (y_prev - y_cur).max(1e-6)) as f64;
                let reach = (i - 1) as f64 * self.ds + self.ds * w;
                let t_prev = self.t[row + i - 1] as f64;
                let t_cur = self.t[row + i] as f64;
                return (reach, t_prev + (t_cur - t_prev) * w);
            }
        }
        let s0 = self.last[k] as f64 * self.ds;
        let y_prev = self.y[row + self.last[k]] as f64 + self.y0;
        if self.sd[k].is_none() && self.end_s[k] > s0 && self.end_y[k] <= y_target && y_prev >= y_target {
            let w = (y_prev - y_target) / (y_prev - self.end_y[k]).max(1e-6);
            let t_prev = self.t[row + self.last[k]] as f64;
            return (s0 + (self.end_s[k] - s0) * w, t_prev + (self.end_t[k] - t_prev) * w);
        }
        (0.0, 0.0)
    }

    pub fn span_05(&self, elev_rad: f64, y_target: f64) -> Option<f64> {
        let reach_at = |ang: f64| -> Option<f64> {
            if ang < self.elev[0] || ang > *self.elev.last()? {
                return None;
            }
            let step = (self.elev[self.ne - 1] - self.elev[0]) / (self.ne - 1) as f64;
            let kf = (ang - self.elev[0]) / step;
            let k0 = (self.ne - 2).min(kf.floor().max(0.0) as usize);
            let w = kf - k0 as f64;
            let r0 = self.reach_row(k0, y_target).0;
            let r1 = self.reach_row(k0 + 1, y_target).0;
            (r0 > 0.0 && r1 > 0.0).then_some(r0 + (r1 - r0) * w)
        };
        let a = reach_at(elev_rad - 0.5 * DEG)?;
        let b = reach_at(elev_rad + 0.5 * DEG)?;
        Some((b - a).abs())
    }

    pub fn envelope(&self, y_target: f64) -> RocketEnvelope {
        let (mut best_s, mut best_deg, mut best_t, mut best_reason, mut capped_by) = (0.0, 0.0, 0.0, "pitch", None);
        for k in 0..self.ne {
            let deg = self.elev[k] / DEG;
            if deg < -20.0 || deg > 50.0 {
                continue;
            }
            let (reach, tof) = self.reach_row(k, y_target);
            if reach == 0.0 && self.end_y[k] > y_target && self.sd[k].is_some() {
                capped_by = self.sd[k];
                continue;
            }
            if reach > best_s {
                best_s = reach;
                best_deg = deg;
                best_t = tof;
                best_reason = self.sd[k].unwrap_or("ground");
            }
        }
        if let Some(c) = capped_by {
            best_reason = c;
        } else if self.t_life > 0.0 && best_t >= self.t_life - 0.5 {
            best_reason = "time";
        } else if self.max_d > 0.0 && best_s.hypot(y_target - self.y0) >= self.max_d - 80.0 {
            best_reason = "dist";
        }
        RocketEnvelope { max_s: best_s, elev_deg: best_deg, tof: best_t, reason: best_reason, t_life: self.t_life, max_dist: self.max_d }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn max_rocket_envelope(sh: &Shell, y0: f64, y_target: f64, p_min: f64, p_max: f64, v_air: f64) -> RocketEnvelope {
    build_traj_fan(sh, y0, p_min.max(-20.0), p_max.min(50.0), v_air, 20000.0, y_target - 50.0).envelope(y_target)
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_table(sh: &Shell, p_min: f64, p_max: f64, step: f64, y_alt: f64, y_floor_abs: f64, max_s: f64, ds: f64) -> Table {
    build_table_air(sh, p_min, p_max, step, y_alt, y_floor_abs, max_s, ds, 0.0)
}

#[allow(clippy::too_many_arguments)]
pub fn build_table_air(sh: &Shell, p_min: f64, p_max: f64, step: f64, y_alt: f64, y_floor_abs: f64, max_s: f64, ds: f64, v_air: f64) -> Table {
    let ne = 2.max(((p_max - p_min) / step + 1e-9).floor() as usize + 1);
    let n = (max_s / ds).ceil() as usize + 1;
    let mut y = vec![f32::NAN; ne * n];
    let (mut apex, mut last, mut elev) = (vec![0; ne], vec![0; ne], vec![0.0; ne]);
    let dt_opt = if sh.rocket.is_some() { 0.004 } else { 0.004f64.min(ds / sh.v0.max(20.0) * 2.0) };
    for k in 0..ne {
        let e = p_max.min(p_min + k as f64 * step) * DEG;
        elev[k] = e;
        let tr = simulate(sh, e, y_alt, max_s + ds, SimOpt { y_floor: Some(y_floor_abs), dt: dt_opt, v_air, ..Default::default() });
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn sweep_reach(t: &Terrain, tab: &Table, a_pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    sweep_reach_ext(t, None, false, tab, a_pt, p, a0, a1)
}

#[allow(clippy::too_many_arguments)]
pub fn sweep_reach_ext(t: &Terrain, outer: Option<&Terrain>, allow_outer: bool, tab: &Table, a_pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    let n = tab.n;
    let y_a = p.y_pt + p.gun_h;
    let mut out = vec![0u8; (a1 - a0) * n];
    let mut prof = vec![0f32; n];
    let mut in_box = vec![false; n];
    for a in a0..a1 {
        let (dx, dz) = ray_dir(a, p.n_az);
        let (mut n_max, mut top, mut entered) = (0usize, f32::NEG_INFINITY, false);
        for i in 0..n {
            let (x, z) = (a_pt.x + dx * i as f64 * tab.ds, a_pt.z + dz * i as f64 * tab.ds);
            if !t.inside(x, z) {
                if !allow_outer && entered {
                    break;
                }
                if allow_outer && outer.is_none_or(|ta| !ta.inside(x, z)) {
                    if entered {
                        break;
                    }
                }
                let og = outer.filter(|ta| ta.inside(x, z)).map_or_else(|| t.height_at(x, z), |ta| ta.height_at(x, z));
                let g_rel = (og - y_a) as f32;
                prof[i] = g_rel;
                if allow_outer && outer.is_some_and(|ta| ta.inside(x, z)) {
                    entered = true;
                    in_box[i] = true;
                    top = top.max(g_rel);
                } else {
                    in_box[i] = false;
                }
                n_max = i + 1;
                continue;
            }
            entered = true;
            in_box[i] = true;
            n_max = i + 1;
            let g = (t.height_at(x, z) - y_a) as f32;
            prof[i] = g;
            top = top.max(g);
        }
        if !entered {
            continue;
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
                if !in_box[i] {
                    if y < prof[i] {
                        break;
                    }
                    i += 1;
                    continue;
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn sweep_hit(t: &Terrain, tab: &Table, b_pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    sweep_hit_ext(t, None, tab, b_pt, p, a0, a1)
}

pub fn sweep_hit_ext(t: &Terrain, outer: Option<&Terrain>, tab: &Table, b_pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    let n = tab.n;
    let y_b = p.y_pt + p.tgt_h;
    let mut out = vec![0u8; (a1 - a0) * n];
    let mut g = vec![0f64; n];
    let mut in_box = vec![false; n];
    let mut sols = vec![];
    for a in a0..a1 {
        let (dx, dz) = ray_dir(a, p.n_az);
        let (mut n_max, mut entered) = (0usize, false);
        for i in 0..n {
            let (x, z) = (b_pt.x + dx * i as f64 * tab.ds, b_pt.z + dz * i as f64 * tab.ds);
            if !t.inside(x, z) {
                if entered {
                    break;
                }
                g[i] = outer.filter(|ta| ta.inside(x, z)).map_or_else(|| t.height_at(x, z), |ta| ta.height_at(x, z));
                in_box[i] = false;
                n_max = i + 1;
                continue;
            }
            entered = true;
            in_box[i] = true;
            n_max = i + 1;
            g[i] = t.height_at(x, z);
        }
        if !entered {
            continue;
        }
        let row0 = (a - a0) * n;
        for j in 1..n_max {
            if !in_box[j] {
                continue;
            }
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn sweep_view(t: &Terrain, pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    sweep_view_ext(t, None, false, pt, p, a0, a1)
}

pub fn sweep_view_ext(t: &Terrain, outer: Option<&Terrain>, allow_outer: bool, pt: Pt, p: &SweepP, a0: usize, a1: usize) -> Vec<u8> {
    let n = (p.range / p.ds).ceil() as usize + 1;
    let y_e = p.y_pt + p.gun_h;
    let mut out = vec![0u8; (a1 - a0) * n];
    for a in a0..a1 {
        let (dx, dz) = ray_dir(a, p.n_az);
        let row0 = (a - a0) * n;
        let (mut max_s, mut hor, mut prev_vis, mut entered) = (f64::NEG_INFINITY, 0usize, true, false);
        for i in 1..n {
            let d = i as f64 * p.ds;
            let (x, z) = (pt.x + dx * d, pt.z + dz * d);
            if !t.inside(x, z) {
                if !allow_outer && entered {
                    break;
                }
                let Some(ta) = outer.filter(|ta| ta.inside(x, z)) else {
                    if entered {
                        break;
                    }
                    let og = t.height_at(x, z);
                    max_s = max_s.max((og - y_e) / d);
                    continue;
                };
                let og = ta.height_at(x, z);
                if allow_outer {
                    entered = true;
                    let top = og + p.tgt_h;
                    let vis = (top - y_e) / d >= max_s;
                    if vis {
                        out[row0 + i] = 1;
                    } else if prev_vis && hor > 0 {
                        let hx = pt.x + dx * hor as f64 * p.ds;
                        let hz = pt.z + dz * hor as f64 * p.ds;
                        out[row0 + hor] = 2 + if t.inside(hx, hz) { t.src_at(hx, hz) } else { 0 };
                    }
                    prev_vis = vis;
                    let sl = (og - y_e) / d;
                    if sl > max_s {
                        max_s = sl;
                        hor = i;
                    }
                } else {
                    max_s = max_s.max((og - y_e) / d);
                }
                continue;
            }
            entered = true;
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn los_blocked(t: &Terrain, a: Pt, y_a: f64, b: Pt, y_b: f64, step: f64) -> Option<f64> {
    los_blocked_with(|x, z| t.height_at(x, z), a, y_a, b, y_b, step)
}

pub fn los_blocked_with(surf: impl Fn(f64, f64) -> f64, a: Pt, y_a: f64, b: Pt, y_b: f64, step: f64) -> Option<f64> {
    let d = (b.x - a.x).hypot(b.z - a.z);
    if d < 1e-6 {
        return None;
    }
    let (dx, dz) = ((b.x - a.x) / d, (b.z - a.z) / d);
    let mut s = step;
    while s < d - 0.5 {
        if surf(a.x + dx * s, a.z + dz * s) > y_a + (y_b - y_a) * s / d {
            return Some(s);
        }
        s += step;
    }
    None
}

/// Exact trajectory vs terrain along dir from a: distance of the first blocking point.
#[allow(dead_code)]
pub fn path_blocked(t: &Terrain, a: Pt, dir: (f64, f64), tr: &Traj, d: f64, step: f64) -> Option<f64> {
    path_blocked_with(|x, z| t.height_at(x, z), a, dir, tr, d, step)
}

pub fn path_blocked_with(surf: impl Fn(f64, f64) -> f64, a: Pt, dir: (f64, f64), tr: &Traj, d: f64, step: f64) -> Option<f64> {
    let mut s = step;
    while s < d - 0.5 {
        let y = tr.at(Key::Y, s);
        if !(y >= surf(a.x + dir.0 * s, a.z + dir.1 * s)) {
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
        Shell { kind: String::new(), type_title: None, v0, m, cal, cx, max_dist: 0.0, name: String::new(), en: None, cx_est: false, rocket: None }
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
            max_dist: 0.0,
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

    #[test]
    fn heli_rocket_s8ko_mach_drag_self_destruct_and_airspeed() {
        let s8 = Shell {
            kind: "heat_fs_rocket".into(),
            type_title: None,
            v0: 650.0,
            m: 11.3,
            cal: 0.08,
            cx: 1.0,
            max_dist: 6200.0,
            name: "S-8KO".into(),
            en: None,
            cx_est: false,
            rocket: Some(RocketMotor {
                v_start: 0.0,
                force0: 7300.0,
                t_fire0: 0.84,
                m_end0: 7.25,
                t_life: 15.0,
                max_dist: 6200.0,
                disp: 1.3,
                cx_mach: true,
                ..Default::default()
            }),
        };
        let env = max_rocket_envelope(&s8, 102.0, 50.0, -25.0, 45.0, 0.0);
        assert_eq!(env.reason, "time");
        assert!(env.max_s > 4900.0 && env.max_s < 5500.0, "env={env:?}");
        let s0 = solve_exact_air(&s8, 4090.8, -35.0, 102.4, -25.0, 45.0, 0.0);
        let s200 = solve_exact_air(&s8, 4090.8, -35.0, 102.4, -25.0, 45.0, 200.0 / 3.6);
        assert!(!s0.is_empty() && !s200.is_empty());
        assert!(s200[0] < s0[0], "forward airspeed lowers required pitch: {} vs {}", s200[0] / DEG, s0[0] / DEG);
        let far = solve_exact_air(&s8, 6000.0, 0.0, 102.4, -25.0, 45.0, 0.0);
        assert!(far.is_empty(), "S-8KO self-destructs at 15s before 6km");

        let fan = build_traj_fan(&s8, 102.4, -25.0, 45.0, 0.0, 12000.0, -500.0);
        for d in [1500.0, 3000.0, 4090.8, 4950.0, 6000.0] {
            let exact = solve_exact_air(&s8, d, -35.0, 102.4, -25.0, 45.0, 0.0);
            let fast = fan.solve(&s8, d, -35.0);
            assert_eq!(exact.len(), fast.len(), "d={d} count mismatch");
            for (ex, (fe, _)) in exact.iter().zip(&fast) {
                assert!((ex - fe).abs() < 0.002 * DEG, "d={d} angle mismatch: {} vs {}", ex / DEG, fe / DEG);
            }
        }
    }
}
