//! Map logic of the page that is not drawing: mission areas per game mode, zone shapes, valid positions,
//! the highest point, aim A -> B and the reach / hit / visibility sweeps.

use rayon::prelude::*;

use crate::core::{self, DEG, Key, Pt, Shell, SweepP, Terrain, Traj};
use crate::data::{Area, LoadedMap, Mission};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameMode {
    Arcade,
    Realistic,
    Simulator,
}
impl GameMode {
    pub const ALL: [GameMode; 3] = [GameMode::Arcade, GameMode::Realistic, GameMode::Simulator];
    pub fn label(self) -> &'static str {
        match self {
            GameMode::Arcade => "街机娱乐",
            GameMode::Realistic => "历史性能",
            GameMode::Simulator => "全真模拟",
        }
    }
    /// Which area variant a mode uses, in order of preference ("" = the unsuffixed area).
    fn order(self) -> [&'static str; 4] {
        match self {
            GameMode::Arcade => ["arcade", "", "realistic", "simulator"],
            GameMode::Realistic => ["realistic", "arcade", "", "simulator"],
            GameMode::Simulator => ["simulator", "", "realistic", "arcade"],
        }
    }
}

/// The mission's areas for a game mode. Mode templates give per mode an area variant (<group>_arcade, ...);
/// without one for the mode they fall back. Areas without mode variants apply to every mode.
pub fn mission_areas(m: &Mission, mode: GameMode) -> Vec<Area> {
    use std::collections::{HashMap, HashSet};
    let key = |a: &Area| format!("{}|{}", a.kind, a.group.as_deref().unwrap_or(&a.name));
    let mut variants: HashMap<String, HashSet<String>> = HashMap::new();
    for a in &m.areas {
        if let Some(md) = &a.mode {
            variants.entry(key(a)).or_default().insert(md.clone());
        }
    }
    for a in &m.areas {
        if a.mode.is_none() {
            if let Some(v) = variants.get_mut(&key(a)) {
                v.insert(String::new());
            }
        }
    }
    m.areas
        .iter()
        .filter(|a| match variants.get(&key(a)) {
            None => true,
            Some(vs) => mode.order().iter().find(|x| vs.contains(**x)).is_some_and(|x| a.mode.as_deref().unwrap_or("") == *x),
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug)]
pub enum Shape {
    Poly([[f64; 2]; 4]),
    Circle { c: [f64; 2], r: f64 },
}

/// Box: unit cube -0.5..0.5 scaled by the tm axes. Sphere / Cylinder: radius = axis length (unverified).
pub fn area_shape(a: &Area) -> Shape {
    let tm = &a.tm;
    let c = [tm[3][0], tm[3][2]];
    if a.shape == "Box" {
        let u = [tm[0][0] / 2.0, tm[0][2] / 2.0];
        let v = [tm[2][0] / 2.0, tm[2][2] / 2.0];
        return Shape::Poly([
            [c[0] - u[0] - v[0], c[1] - u[1] - v[1]],
            [c[0] + u[0] - v[0], c[1] + u[1] - v[1]],
            [c[0] + u[0] + v[0], c[1] + u[1] + v[1]],
            [c[0] - u[0] + v[0], c[1] - u[1] + v[1]],
        ]);
    }
    let r = if a.shape == "Sphere" || a.shape == "Cylinder" { tm[0][0].hypot(tm[0][2]) } else { 0.0 };
    Shape::Circle { c, r }
}

pub fn in_shape(sh: &Shape, x: f64, z: f64) -> bool {
    match sh {
        Shape::Poly(p) => {
            let mut inside = false;
            let mut j = p.len() - 1;
            for i in 0..p.len() {
                let ([xi, zi], [xj, zj]) = (p[i], p[j]);
                if (zi > z) != (zj > z) && x < (xj - xi) * (z - zi) / (zj - zi) + xi {
                    inside = !inside;
                }
                j = i;
            }
            inside
        }
        Shape::Circle { c, r } => (x - c[0]).hypot(z - c[1]) <= *r,
    }
}

impl Shape {
    /// x0, z0, x1, z1
    pub fn bbox(&self) -> [f64; 4] {
        match self {
            Shape::Poly(p) => p.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, q| [b[0].min(q[0]), b[1].min(q[1]), b[2].max(q[0]), b[3].max(q[1])]),
            Shape::Circle { c, r } => [c[0] - r, c[1] - r, c[0] + r, c[1] + r],
        }
    }
}

/// Outline colour of an area (0xRRGGBB).
pub fn area_color(a: &Area) -> u32 {
    match a.kind.as_str() {
        "battle" => 0xffffff,
        "capture" => 0xf2d14b,
        "kill" => 0xe2574c,
        _ => match a.team {
            Some(1) => 0x4fb3e8,
            Some(2) => 0xe98a3c,
            _ => 0xaaaaaa,
        },
    }
}

/// Battle areas and kill areas (red zones) of the selected mission and mode.
#[derive(Default, Clone)]
pub struct Zones {
    pub battle: Vec<Shape>,
    pub kill: Vec<Shape>,
}
impl Zones {
    pub fn of(areas: &[Area]) -> Self {
        Self {
            battle: areas.iter().filter(|a| a.kind == "battle").map(area_shape).collect(),
            kill: areas.iter().filter(|a| a.kind == "kill").map(area_shape).collect(),
        }
    }
    /// Inside the battle area (if any) and outside every kill zone.
    pub fn valid(&self, x: f64, z: f64) -> bool {
        (self.battle.is_empty() || self.battle.iter().any(|s| in_shape(s, x, z))) && !self.kill.iter().any(|s| in_shape(s, x, z))
    }
}

/// Highest bare ground a tank may stand on: inside the battle area (else the map bounds), outside every red zone.
pub fn highest_point(map: &LoadedMap, zones: &Zones) -> Option<(f64, f64, f64)> {
    let t = &map.t;
    let c = t.cell;
    let [mut x0, mut z0, mut x1, mut z1] = map.meta.bounds.unwrap_or([t.ox, t.oz, t.x1, t.z1]);
    if !zones.battle.is_empty() {
        let b = zones.battle.iter().map(Shape::bbox).fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, q| [a[0].min(q[0]), a[1].min(q[1]), a[2].max(q[2]), a[3].max(q[3])]);
        [x0, z0, x1, z1] = b;
    }
    let i0 = ((x0 - t.ox) / c).ceil().max(0.0) as usize;
    let i1 = (((x1 - t.ox) / c).floor().max(-1.0) as isize).min(t.w as isize - 1);
    let j0 = ((z0 - t.oz) / c).ceil().max(0.0) as usize;
    let j1 = (((z1 - t.oz) / c).floor().max(-1.0) as isize).min(t.hh as isize - 1);
    let mut best: Option<(f64, f64, f64)> = None;
    for j in j0 as isize..=j1 {
        for i in i0 as isize..=i1 {
            let h = t.h[j as usize * t.w + i as usize] as f64;
            if best.is_some_and(|b| h <= b.2) {
                continue; // shape tests only for a new maximum
            }
            let (x, z) = (t.ox + i as f64 * c, t.oz + j as f64 * c);
            if zones.valid(x, z) {
                best = Some((x, z, h));
            }
        }
    }
    best
}

// ------------------------------------------------------------------ tank hull on the surface

/// Footprint of a tank hull (m) for its pose on the ground: about a modern MBT's track length and width.
pub const HULL_LEN: f64 = 6.5;
pub const HULL_W: f64 = 3.4;

/// Pose of a tank at `p` heading along the unit vector `dir` (x, z): its forward, up and side axes (the rows of
/// the model's transform; the model faces +X) and the height of its base. Pitch and roll follow the surface where
/// tanks stand (ground, rocks, objects) under the hull's front and back and its two sides.
pub fn hull_pose(map: &LoadedMap, p: Pt, dir: (f64, f64)) -> ([f64; 3], [f64; 3], [f64; 3], f64) {
    let (dx, dz) = dir;
    let (sx, sz) = (-dz, dx); // the model's +Z side
    let (l2, w2) = (HULL_LEN / 2.0, HULL_W / 2.0);
    let h = |f: f64, s: f64| map.stand(p.x + dx * f + sx * s, p.z + dz * f + sz * s);
    let (front, back, side, other) = (h(l2, 0.0), h(-l2, 0.0), h(0.0, w2), h(0.0, -w2));
    let norm = |v: [f64; 3]| { let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt(); v.map(|c| c / l) };
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let fwd = norm([dx * HULL_LEN, front - back, dz * HULL_LEN]);
    let up = norm(cross([sx * HULL_W, side - other, sz * HULL_W], fwd));
    let base = map.stand(p.x, p.z).max((front + back + side + other) / 4.0); // on a crest the hull rests on its middle
    (fwd, up, cross(fwd, up), base)
}

// ------------------------------------------------------------------ aim A -> B

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub p_min: f64,
    pub p_max: f64,
    pub range: f64,
    pub gun_h: f64,
    pub tgt_h: f64,
    pub v_air: f64,
}

pub struct Sol {
    pub e: f64,
    pub tr: Traj,
    pub t: f64,
    pub v: f64,
    pub blocked: Option<f64>,
    pub by: &'static str,
}
pub struct Aim {
    pub d: f64,
    pub dy: f64,
    pub y_a: f64,
    pub y_b: f64,
    pub g_a: f64,
    pub g_b: f64,
    pub los: Option<f64>,
    pub los_by: &'static str,
    pub sols: Vec<Sol>,
    pub dir: (f64, f64),
}

fn what(t: &Terrain, a: Pt, dir: (f64, f64), s: f64) -> &'static str {
    let (x, z) = (a.x + dir.0 * s, a.z + dir.1 * s);
    if !t.inside(x, z) {
        return "外圈地形";
    }
    ["地形", "建筑/岩石掩体", "树木/灌木"][t.src_at(x, z).min(2) as usize]
}

/// tc: what shots collide with, ts: what blocks sight.
pub fn compute_aim(map: &LoadedMap, tc: &Terrain, ts: &Terrain, shell: &Shell, a: Pt, b: Pt, p: &Params) -> Aim {
    let t = &map.t;
    let (g_a, g_b) = (map.stand(a.x, a.z), map.stand(b.x, b.z));
    let (y_a, y_b) = (g_a + p.gun_h, g_b + p.tgt_h);
    let d = (b.x - a.x).hypot(b.z - a.z);
    let dy = y_b - y_a;
    let dir = if d > 0.0 { ((b.x - a.x) / d, (b.z - a.z) / d) } else { (0.0, 1.0) };
    let step = if d > 6000.0 { t.cell.max(3.0) } else { t.cell / 2.0 };
    let los = core::los_blocked_with(|x, z| map.surf_at(ts, x, z), a, y_a, b, y_b, step);
    let sols = if d > 1.0 {
        core::solve_exact_air(shell, d, dy, y_a, p.p_min, p.p_max, p.v_air)
            .into_iter()
            .map(|e| {
                let tr = core::simulate(shell, e, y_a, d + 2.0, core::SimOpt { v_air: p.v_air, ..Default::default() });
                let blocked = core::path_blocked_with(|x, z| map.surf_at(tc, x, z), a, dir, &tr, d, step);
                Sol { e, t: tr.at(Key::T, d), v: tr.at(Key::V, d), by: blocked.map_or("", |s| what(tc, a, dir, s)), blocked, tr }
            })
            .collect()
    } else {
        vec![]
    };
    Aim { d, dy, y_a, y_b, g_a, g_b, los, los_by: los.map_or("", |s| what(ts, a, dir, s)), sols, dir }
}

pub fn mil(r: f64) -> String {
    format!("{:.1}", r * 3200.0 / std::f64::consts::PI)
}

pub fn compass_name(az_deg: f64) -> &'static str {
    const DIRS: [&str; 16] = ["正北 N", "北偏东 NNE", "东北 NE", "东偏北 ENE", "正东 E", "东偏南 ESE", "东南 SE", "南偏东 SSE", "正南 S", "南偏西 SSW", "西南 SW", "西偏南 WSW", "正西 W", "西偏北 WNW", "西北 NW", "北偏西 NNW"];
    DIRS[(((az_deg % 360.0) + 360.0) % 360.0 / 22.5).round() as usize % 16]
}

pub fn capture_zones_labeled(areas: &[Area]) -> Vec<(String, f64, f64)> {
    let caps: Vec<&Area> = areas.iter().filter(|a| a.kind == "capture").collect();
    caps.iter()
        .enumerate()
        .map(|(i, a)| {
            let nl = a.name.to_lowercase();
            let letter = if nl.contains("_01") || nl.ends_with("_a") {
                "A".to_string()
            } else if nl.contains("_02") || nl.ends_with("_b") {
                "B".to_string()
            } else if nl.contains("_03") || nl.ends_with("_c") {
                "C".to_string()
            } else if nl.contains("_04") || nl.ends_with("_d") {
                "D".to_string()
            } else {
                char::from_u32('A' as u32 + i as u32).unwrap_or('Z').to_string()
            };
            let label = if caps.len() == 1 { "战区点 A".into() } else { format!("战区 {letter}") };
            (label, a.tm[3][0], a.tm[3][2])
        })
        .collect()
}

pub fn tank_spawn_clusters(areas: &[Area]) -> Vec<(String, f64, f64)> {
    let mut out = vec![];
    for team in [1i64, 2i64] {
        let sp: Vec<&Area> = areas.iter().filter(|a| a.kind == "spawn" && a.team == Some(team)).collect();
        if sp.is_empty() {
            continue;
        }
        let mut clusters: Vec<Vec<&Area>> = vec![];
        for s in sp {
            let (sx, sz) = (s.tm[3][0], s.tm[3][2]);
            if let Some(cl) = clusters.iter_mut().find(|c| {
                let cx = c.iter().map(|p| p.tm[3][0]).sum::<f64>() / c.len() as f64;
                let cz = c.iter().map(|p| p.tm[3][2]).sum::<f64>() / c.len() as f64;
                (sx - cx).hypot(sz - cz) < 260.0
            }) {
                cl.push(s);
            } else {
                clusters.push(vec![s]);
            }
        }
        for (idx, cl) in clusters.iter().enumerate() {
            let cx = cl.iter().map(|p| p.tm[3][0]).sum::<f64>() / cl.len() as f64;
            let cz = cl.iter().map(|p| p.tm[3][2]).sum::<f64>() / cl.len() as f64;
            let sfx = if clusters.len() > 1 { format!("#{}", idx + 1) } else { String::new() };
            out.push((format!("{team}队陆战出生{sfx}"), cx, cz));
        }
    }
    out
}

#[allow(dead_code)]
fn pitch_span_05(sh: &Shell, e_rad: f64, y_a: f64, y_target: f64, v_air: f64) -> Option<f64> {
    let hit = |ang: f64| -> Option<f64> {
        let tr = core::simulate(sh, ang, y_a, 30000.0, core::SimOpt { y_floor: Some(y_target - 20.0), v_air, ..Default::default() });
        for k in 1..tr.s.len() {
            if tr.y[k] <= y_target && tr.y[k - 1] >= y_target {
                let w = (tr.y[k - 1] - y_target) / (tr.y[k - 1] - tr.y[k]).max(1e-6);
                return Some(tr.s[k - 1] + (tr.s[k] - tr.s[k - 1]) * w);
            }
        }
        None
    };
    let s1 = hit(e_rad - 0.5 * DEG)?;
    let s2 = hit(e_rad + 0.5 * DEG)?;
    Some((s2 - s1).abs())
}

/// Helicopter firing table & HUD lines for the desktop Aim card, plus cached rocket max envelope (m).
#[allow(clippy::too_many_arguments)]
pub fn heli_aim_lines(map: &LoadedMap, tc: &Terrain, areas: &[Area], shell: &Shell, a: Pt, aim: Option<&Aim>, p: &Params) -> (Vec<(String, u8)>, Option<f64>) {
    let mut out = vec![];
    let g_a = map.stand(a.x, a.z);
    let y_a = g_a + p.gun_h;
    let caps = capture_zones_labeled(areas);
    let ref_ground = caps.first().map_or((map.t.lo + map.t.hi) * 0.5, |c| map.stand(c.1, c.2));
    let floor_y = ref_ground.min(map.t.lo) - 180.0;
    let fan = core::build_traj_fan(shell, y_a, p.p_min, p.p_max, p.v_air, 13000.0, floor_y);
    let env = shell.rocket.map(|_| fan.envelope(ref_ground));
    let spd_kmh = p.v_air * 3.6;

    out.push((format!("机位高度  离地 +{:.0} m  |  海拔 {:.0} m  |  {}", p.gun_h, y_a, if spd_kmh > 0.5 { format!("空速 {:.0} km/h", spd_kmh) } else { "悬停 0 km/h".into() }), 0));
    if let Some(ev) = env {
        let cap_note = match ev.reason {
            "time" => format!("受 {:.0}s 自毁限制", ev.t_life),
            "dist" => format!("受 {:.1}km 自毁限制", ev.max_dist / 1000.0),
            _ => "当前俯仰包线".into(),
        };
        out.push((format!("极限射程  约 {:.0} m（{}，最佳仰角 {:+.1}°）", ev.max_s, cap_note, ev.elev_deg), 0));
    }

    if let Some(aim) = aim {
        let az = (aim.dir.0.atan2(aim.dir.1) / DEG + 360.0) % 360.0;
        out.push((String::new(), 0));
        if let Some(q) = aim.sols.iter().find(|s| s.blocked.is_none()).or_else(|| aim.sols.first()) {
            let deg = q.e / DEG;
            out.push((format!("★ 罗盘方位角  {:05.1}°（{}）", az, compass_name(az)), 1));
            out.push((format!("★ 机头俯仰角  {:+.2}°（{} · {} 密位）", deg, if deg >= 0.0 { "抬头" } else { "压头" }, mil(q.e)), 1));
            match q.blocked {
                None => out.push((format!("  弹道畅通可命中 · 水平 {:.0} m（斜距 {:.0} m，高差 {:+.0} m）", aim.d, aim.d.hypot(aim.dy), aim.dy), 1)),
                Some(s) => out.push((format!("  警告：在 {s:.0} m 处被{}遮挡！", q.by), 2)),
            }
            let mut life = format!("  飞行 {:.2} s · 落速 {:.0} m/s", q.t, q.v);
            if let Some(rm) = shell.rocket {
                if rm.t_life > 0.0 {
                    life.push_str(&format!(" · 距自毁余 {:.1} s", (rm.t_life - q.t).max(0.0)));
                } else if rm.max_dist > 0.0 {
                    life.push_str(&format!(" · 距自毁余 {:.2} km", ((rm.max_dist - aim.d.hypot(aim.dy)) / 1000.0).max(0.0)));
                }
            }
            out.push((life, 0));
            if let Some(sp05) = fan.span_05(q.e, aim.y_b) {
                let disp_txt = shell.rocket.filter(|r| r.disp > 0.0).map_or(String::new(), |r| format!(" · 散布 ±{:.0} m", aim.d * (r.disp * 0.45 * DEG).tan()));
                out.push((format!("  ±0.5° 仰角齐射覆盖 ≈ {:.0} m{disp_txt}", sp05), 0));
            }
        } else {
            out.push((format!("★ 罗盘方位角  {:05.1}°（{}）   机头仰角：不可达", az, compass_name(az)), 2));
            if let Some(ev) = env.filter(|ev| ev.max_s > 0.0 && aim.d > ev.max_s) {
                let short = (aim.d - ev.max_s).round();
                let why = match ev.reason {
                    "time" => format!("飞满 {:.0}s 强制半空自爆", ev.t_life),
                    "dist" => format!("满 {:.1}km 最大距离自爆", ev.max_dist / 1000.0),
                    _ => "超出射程".into(),
                };
                out.push((format!("  【超距自毁】{}，最远约 {:.0} m（差 {:.0} m，请前移或提速）", why, ev.max_s, short), 2));
            } else {
                out.push((format!("  当前距离 {:.0} m 超出射界 {}..{}°", aim.d, p.p_min, p.p_max), 2));
            }
        }
    } else {
        out.push(("右键点击地图放置目标点 B，或直接参考下方战区速查射表：".into(), 0));
    }

    // Multi-target tactical table (Captures + Spawns) solved from the single pre-built TrajFan
    let mut rows = caps;
    rows.extend(tank_spawn_clusters(areas));
    if !rows.is_empty() {
        out.push((String::new(), 0));
        out.push(("── 本局关键点位速查射表（方位 / 机头仰角 / 距离 / 飞行）──".into(), 0));
        let step = map.t.cell.max(8.0);
        for (label, tx, tz) in rows {
            let d = (tx - a.x).hypot(tz - a.z);
            let az = ((tx - a.x).atan2(tz - a.z) / DEG + 360.0) % 360.0;
            let y_b = map.stand(tx, tz) + p.tgt_h;
            let sols = if d > 5.0 { fan.solve(shell, d, y_b - y_a) } else { vec![] };
            let mut best: Option<(f64, f64, Option<f64>)> = None;
            for (e, tof) in sols {
                let tr = core::simulate(shell, e, y_a, d + 2.0, core::SimOpt { dt: 0.012, v_air: p.v_air, ..Default::default() });
                let bl = core::path_blocked_with(|x, z| map.surf_at(tc, x, z), a, ((tx - a.x) / d, (tz - a.z) / d), &tr, d, step);
                if bl.is_none() {
                    best = Some((e, tof, None));
                    break;
                } else if best.is_none() {
                    best = Some((e, tof, bl));
                }
            }
            match best {
                Some((e, tof, None)) => out.push((format!("{label:<8}  方位 {:05.1}°  仰角 {:+6.2}°  {:.0}m  {:.1}s  可达", az, e / DEG, d, tof), 1)),
                Some((e, tof, Some(_))) => out.push((format!("{label:<8}  方位 {:05.1}°  仰角 {:+6.2}°  {:.0}m  {:.1}s  受阻", az, e / DEG, d, tof), 2)),
                None => {
                    let tag = if env.is_some_and(|ev| d > ev.max_s && matches!(ev.reason, "time" | "dist")) { "自毁超距" } else { "超射程" };
                    out.push((format!("{label:<8}  方位 {:05.1}°  仰角    —    {:.0}m   —   {tag}", az, d), 2));
                }
            }
        }
    }

    // Compact range ladder every 500m (0 extra trajectory simulations — tof comes straight from fan.solve!)
    let dy_ref = ref_ground - y_a;
    let max_l = env.map_or(6000.0, |ev| (ev.max_s / 500.0).ceil() * 500.0).clamp(1500.0, 10000.0);
    let mut ladder = vec![];
    let mut dist = 1000.0;
    while dist <= max_l {
        if let Some(&(e, tof)) = fan.solve(shell, dist, dy_ref).first() {
            ladder.push(format!("{:.1}km:{:+.2}°({:.1}s)", dist / 1000.0, e / DEG, tof));
        }
        dist += 500.0;
    }
    if !ladder.is_empty() {
        out.push((String::new(), 0));
        out.push(("── 距离梯度速查（按战场基准海拔）──".into(), 0));
        for ch in ladder.chunks(2) {
            out.push((ch.join("   |   "), 0));
        }
    }
    (out, env.map(|e| e.max_s).filter(|&s| s > 200.0))
}

/// The aim card text, one (text, kind) per line: kind 0 normal, 1 good, 2 bad.
pub fn aim_lines(aim: &Aim, p: &Params) -> Vec<(String, u8)> {
    let mut out = vec![];
    let los_ang = aim.dy.atan2(aim.d);
    let az = (aim.dir.0.atan2(aim.dir.1) / DEG + 360.0) % 360.0;
    out.push((format!("水平距离  {:.1} m（斜距 {:.1} m）", aim.d, aim.d.hypot(aim.dy)), 0));
    out.push((format!("海拔高差  A {:.1}  B {:.1}  Δ {}{:.1} m", aim.g_a, aim.g_b, if aim.dy >= 0.0 { "+" } else { "" }, aim.dy), 0));
    out.push((format!("方位角    {az:.1}°   视线高低角 {:.2}°", los_ang / DEG), 0));
    match aim.los {
        None => out.push(("视线畅通（无遮挡）".into(), 1)),
        Some(s) => out.push((format!("视线在 {s:.0} m 处被{}遮挡", aim.los_by), 2)),
    }
    if aim.d > 1.0 && aim.sols.is_empty() {
        out.push((format!("俯仰角 {}..{}° 内无射击诸元（超出射程或仰角不足）", p.p_min, p.p_max), 2));
    }
    for (i, q) in aim.sols.iter().enumerate() {
        let hold = aim.d * q.e.tan() - aim.dy;
        let name = if aim.sols.len() > 1 { if i > 0 { "高抛弹道（曲射）" } else { "低伸弹道（直射）" } } else { "弹道诸元" };
        out.push((String::new(), 0));
        match q.blocked {
            None => out.push((format!("{name}：可命中目标"), 1)),
            Some(s) => out.push((format!("{name}：在 {s:.0} m 处被{}阻挡", q.by), 2)),
        }
        out.push((format!("  火炮仰角  {:.3}°（{} 密位）", q.e / DEG, mil(q.e)), 0));
        out.push((format!("  瞄准抬高  {} 密位 = 瞄在 B 上方 {hold:.2} m", mil(q.e - los_ang)), 0));
        out.push((format!("  飞行时间  {:.2} s，落点存速 {:.0} m/s", q.t, q.v), 0));
    }
    out
}

// ------------------------------------------------------------------ sweeps

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SweepKind {
    Reach,
    Hit,
    View,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Perf {
    Fast,
    Balanced,
    Precise,
}
impl Perf {
    pub const ALL: [Perf; 3] = [Perf::Fast, Perf::Balanced, Perf::Precise];
    /// rays, distance step m, elevation step deg
    pub fn values(self) -> (usize, f64, f64) {
        match self {
            Perf::Fast => (360, 8.0, 0.1),
            Perf::Balanced => (720, 4.0, 0.05),
            Perf::Precise => (1440, 2.0, 0.02),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Perf::Fast => "快速：360 方向, 8 m, 0.1°",
            Perf::Balanced => "均衡：720 方向, 4 m, 0.05°",
            Perf::Precise => "精细：1440 方向, 2 m, 0.02°",
        }
    }
}

/// A sweep result drawn over the map: square RGBA image (+Z up) covering x0..x0+size, z0..z0+size.
pub struct Overlay {
    pub kind: SweepKind,
    pub img: image::RgbaImage,
    pub x0: f64,
    pub z0: f64,
    pub size: f64,
    pub km2: f64,
}

/// Runs a sweep on all cores (rayon, one chunk of rays per task) and rasterises it like finishSweep().
/// t: the surface the sweep collides with (tc for reach/hit, ts for view); zones: None = no clipping.
#[allow(clippy::too_many_arguments)]
pub fn run_sweep(kind: SweepKind, map: &LoadedMap, t: &Terrain, shell: &Shell, pt: Pt, p: &Params, perf: Perf, zones: Option<&Zones>) -> Overlay {
    let (n_az, ds, e_step) = perf.values();
    let y_pt = map.stand(pt.x, pt.z);
    let sp = SweepP { n_az, ds, range: p.range, gun_h: p.gun_h, tgt_h: p.tgt_h, y_pt };
    let tab = (kind != SweepKind::View).then(|| {
        let y_alt = y_pt + if kind == SweepKind::Reach { p.gun_h } else { p.tgt_h };
        core::build_table_air(shell, p.p_min, p.p_max, e_step, y_alt, t.lo - 200.0, p.range, ds, p.v_air)
    });
    let n = tab.as_ref().map_or((p.range / ds).ceil() as usize + 1, |t| t.n);
    let chunk = 8;
    let parts: Vec<Vec<u8>> = (0..n_az.div_ceil(chunk))
        .into_par_iter()
        .map(|c| {
            let (a0, a1) = (c * chunk, ((c + 1) * chunk).min(n_az));
            let outer = map.t_air.as_deref();
            match (kind, &tab) {
                (SweepKind::Reach, Some(tab)) => core::sweep_reach_ext(t, outer, tab, pt, &sp, a0, a1),
                (SweepKind::Hit, Some(tab)) => core::sweep_hit(t, tab, pt, &sp, a0, a1),
                _ => core::sweep_view_ext(t, outer, pt, &sp, a0, a1),
            }
        })
        .collect();
    let all = parts.concat();

    let r = (n - 1) as f64 * ds;
    let nn = 2048.min((2.0 * r / ds).ceil() as u32);
    let px = 2.0 * r / nn as f64;
    let view = kind == SweepKind::View;
    let col = |v: u8| -> [u8; 4] {
        if view {
            match v {
                0 => [0, 0, 0, 110],
                1 => [88, 196, 114, 110],
                2 => [226, 60, 50, 235],
                3 => [240, 150, 40, 235],
                _ => [250, 230, 60, 235],
            }
        } else if kind == SweepKind::Reach {
            [88, 196, 114, 120]
        } else {
            [[0, 0, 0, 0], [88, 196, 114, 120], [233, 138, 60, 120], [79, 179, 232, 120]][v.min(3) as usize]
        }
    };
    let rows: Vec<(Vec<u8>, u32)> = (0..nn)
        .into_par_iter()
        .map(|iy| {
            let mut row = vec![0u8; nn as usize * 4];
            let mut cells = 0;
            for ix in 0..nn {
                let dx = (ix as f64 + 0.5) * px - r;
                let dz = r - (iy as f64 + 0.5) * px;
                let i = (dx.hypot(dz) / ds).round() as usize;
                if i >= n || i == 0 {
                    continue;
                }
                let mut az = dx.atan2(dz);
                if az < 0.0 {
                    az += 2.0 * std::f64::consts::PI;
                }
                let v = all[((az / (2.0 * std::f64::consts::PI) * n_az as f64).round() as usize % n_az) * n + i];
                let (x, z) = (pt.x + dx, pt.z + dz);
                if (v == 0 && !view) || !map.t.inside(x, z) || (!view && zones.is_some_and(|zs| !zs.valid(x, z))) {
                    continue;
                }
                if !view && map.tr.src_at(x, z) == 1 {
                    continue; // no tank inside a building
                }
                row[ix as usize * 4..ix as usize * 4 + 4].copy_from_slice(&col(v));
                if v != 0 {
                    cells += 1;
                }
            }
            (row, cells)
        })
        .collect();
    let cells: u32 = rows.iter().map(|r| r.1).sum();
    let img = image::RgbaImage::from_raw(nn, nn, rows.into_iter().flat_map(|r| r.0).collect()).expect("overlay size");
    Overlay { kind, img, x0: pt.x - r, z0: pt.z - r, size: 2.0 * r, km2: cells as f64 * px * px / 1e6 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(name: &str, kind: &str, mode: Option<&str>) -> Area {
        Area { name: name.into(), kind: kind.into(), team: None, shape: "Box".into(), tm: [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 2.0], [0.0, 0.0, 0.0]], mode: mode.map(Into::into), group: Some(name.split('_').next().unwrap().into()) }
    }

    #[test]
    fn mode_variants_fall_back_like_the_page() {
        let m = Mission { name: "m".into(), title: None, areas: vec![area("kill_a", "kill", None), area("kill_a", "kill", Some("simulator")), area("battle_x", "battle", Some("arcade"))] };
        let names = |md| mission_areas(&m, md).iter().map(|a| a.mode.clone().unwrap_or_default()).collect::<Vec<_>>();
        assert_eq!(names(GameMode::Simulator), ["simulator", "arcade"]);
        assert_eq!(names(GameMode::Realistic), ["", "arcade"]); // realistic falls back to arcade, then ""
    }

    #[test]
    fn box_shape_and_inside() {
        let s = area_shape(&area("b", "battle", None));
        assert!(in_shape(&s, 0.5, 0.5));
        assert!(!in_shape(&s, 1.5, 0.0));
    }

    #[test]
    fn highest_point_matches_the_page() {
        let dir = crate::data::Store::open();
        let Ok((meta, h16)) = crate::data::load_map(&dir, "avg_american_valley") else { return };
        let map = LoadedMap::new(meta, h16);
        for m in &map.meta.missions {
            for mode in GameMode::ALL {
                let (_, _, h) = highest_point(&map, &Zones::of(&mission_areas(m, mode))).unwrap();
                assert_eq!(format!("{h:.2}"), "61.02", "{} {mode:?}", m.name); // checked against a scan of every heightmap cell
            }
        }
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    /// A in the map border zone: some ground is reachable, none of it inside the battle area outside red zones.
    #[test]
    fn reach_on_american_valley() {
        let dir = crate::data::Store::open();
        let Ok((meta, h16)) = crate::data::load_map(&dir, "avg_american_valley") else { return };
        let map = LoadedMap::new(meta, h16);
        let idx = crate::data::load_index(&dir).unwrap();
        let sh = idx.tanks.iter().find(|t| t.id == "us_m3_stuart").unwrap().guns[0].shells[0].clone();
        let areas = mission_areas(map.meta.missions.iter().find(|m| m.name.ends_with("_dom")).unwrap(), GameMode::Realistic);
        let cap = areas.iter().find(|a| a.kind == "capture").unwrap();
        let pt = Pt { x: cap.tm[3][0] - 600.0, z: cap.tm[3][2] - 200.0 };
        let p = Params { p_min: -10.0, p_max: 20.0, range: 2500.0, gun_h: 2.0, tgt_h: 1.5, v_air: 0.0 };
        let all = run_sweep(SweepKind::Reach, &map, &map.tr, &sh, pt, &p, Perf::Fast, None);
        let z = Zones::of(&areas);
        let clip = run_sweep(SweepKind::Reach, &map, &map.tr, &sh, pt, &p, Perf::Fast, Some(&z));
        assert!(!z.valid(pt.x, pt.z));
        assert!(all.km2 > 0.1, "{}", all.km2);
        assert!(clip.km2 < all.km2);
    }
}

#[cfg(test)]
mod stand_tests {
    use super::*;
    /// A and B clicked onto a rock or other solid object stand on its top, not on the ground inside it.
    #[test]
    fn points_stand_on_objects() {
        let store = crate::data::Store::open();
        let Ok((meta, h16)) = crate::data::load_map(&store, "avg_normandy") else { return };
        let mut map = LoadedMap::new(meta, h16);
        map.set_objects(crate::data::load_objects(&store, "avg_normandy").unwrap()).unwrap();
        let (t, tr) = (&map.t, &map.tr);
        let i = (0..t.h.len()).find(|&i| tr.h[i] - t.h[i] > 4.0).expect("an object on the map");
        let a = Pt { x: t.ox + (i % t.w) as f64 * t.cell, z: t.oz + (i / t.w) as f64 * t.cell };
        let b = Pt { x: a.x + 300.0, z: a.z };
        let idx = crate::data::load_index(&store).unwrap();
        let sh = &idx.tanks[0].guns[0].shells[0];
        let p = Params { p_min: -10.0, p_max: 20.0, range: 2500.0, gun_h: 2.0, tgt_h: 1.5, v_air: 0.0 };
        let aim = compute_aim(&map, tr, tr, sh, a, b, &p);
        assert!((aim.g_a - tr.height_at(a.x, a.z)).abs() < 1e-9);
        assert!(aim.g_a > t.height_at(a.x, a.z) + 3.0);
        assert!((aim.y_a - aim.g_a - 2.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod pose_tests {
    use super::*;
    use crate::data::MapMeta;

    /// A hillside rising 10 % towards +Z: a tank heading up it pitches by atan(0.1), across it rolls by the same.
    #[test]
    fn hull_follows_the_slope() {
        let (w, cell) = (101, 2.0);
        let h16: Vec<u16> = (0..w * w).map(|i| (((0.1 * (i / w) as f64 * cell) + 50.0) / 250.0 * 65535.0).round() as u16).collect();
        let meta = MapMeta { name: "slope".into(), bounds: None, width: w, height: w, cell, ox: 0.0, oz: 0.0, hmin: -50.0, hmax: 200.0, missions: vec![], water: None, helipads: vec![], air: None };
        let map = LoadedMap::new(meta, h16);
        let p = Pt { x: 100.0, z: 100.0 };
        let (f, u, s, base) = hull_pose(&map, p, (0.0, 1.0));
        assert!((f[1].atan2(f[2]) - 0.1f64.atan()).abs() < 1e-3, "pitch {f:?}");
        assert!((s[1]).abs() < 1e-6 && u[1] > 0.99);
        assert!((base - 10.0).abs() < 0.05, "base {base}");
        let (f, u, s, _) = hull_pose(&map, p, (1.0, 0.0));
        assert!(f[1].abs() < 1e-6, "no pitch across the slope");
        assert!((s[1].abs() - (0.1f64.atan()).sin()).abs() < 1e-3, "roll {s:?}");
        assert!(u[1] > 0.99);
        let (f, ..) = hull_pose(&map, p, (0.0, -1.0));
        assert!(f[1] < 0.0, "nose down going downhill");
    }
}

#[cfg(test)]
mod reach_clip_tests {
    use super::*;
    /// Reach clipped to the battle area only covers ground inside red zones that clipping by them removes.
    #[test]
    fn reach_reaches_into_red_zones() {
        let store = crate::data::Store::open();
        let Ok((meta, h16)) = crate::data::load_map(&store, "avg_american_valley") else { return };
        let map = LoadedMap::new(meta, h16);
        let idx = crate::data::load_index(&store).unwrap();
        let sh = idx.tanks.iter().find(|t| t.id == "germ_leopard_2a4").unwrap().guns[0].shells[0].clone();
        let areas = mission_areas(map.meta.missions.iter().find(|m| m.name.ends_with("_dom")).unwrap(), GameMode::Realistic);
        let full = Zones::of(&areas);
        assert!(!full.kill.is_empty());
        let mut battle = full.clone();
        battle.kill.clear();
        let (x, z, _) = highest_point(&map, &full).unwrap();
        let p = Params { p_min: -10.0, p_max: 20.0, range: 2500.0, gun_h: 2.0, tgt_h: 1.5, v_air: 0.0 };
        let a = run_sweep(SweepKind::Reach, &map, &map.tr, &sh, Pt { x, z }, &p, Perf::Fast, Some(&battle));
        let b = run_sweep(SweepKind::Reach, &map, &map.tr, &sh, Pt { x, z }, &p, Perf::Fast, Some(&full));
        assert!(a.km2 > b.km2 + 0.01, "battle only {} vs without red zones {}", a.km2, b.km2);
    }
}

