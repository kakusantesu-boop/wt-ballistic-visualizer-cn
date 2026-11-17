//! 2D map and side profile (the page's draw() and drawProfile()).

use gpui::{App, Bounds, Pixels, Window, point, px};

use crate::app::{COL_A, COL_B, COL_BPT, COL_HIGH, Sw, Viewer};
use crate::core::Key;
use crate::paint::{self, color, p};
use crate::scene::{self, Shape};

pub fn map(v: &mut Viewer, b: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    window.paint_quad(gpui::fill(b, color(0x0c0e10, 1.0)));
    let Some(m) = v.map.clone() else { return };
    let t = &m.t;
    let c = t.cell;
    // a screen rect for a world extent starting at the map's top-left corner (x0, z top)
    let rect = |v: &Viewer, x0: f64, ztop: f64, wm: f64, hm: f64| Bounds::new(point(px(ox + v.sx(x0)), px(oy + v.sy(ztop))), gpui::size(px((wm * v.view.s) as f32), px((hm * v.view.s) as f32)));
    let ext = (t.w as f64 * c, t.hh as f64 * c);
    if let (Some(ta), Some(la)) = (&m.t_air, &v.air_img) {
        let ca = ta.cell;
        let ra = rect(v, ta.ox - ca / 2.0, ta.z1 + ca / 2.0, ta.w as f64 * ca, ta.hh as f64 * ca);
        paint::image(window, &la.img, ra);
        if v.sw.on(Sw::Water) {
            if let Some(wa) = &v.air_water {
                paint::image(window, &wa.img, ra);
            }
        }
    }
    if let Some(l) = &v.map_img {
        paint::image(window, &l.img, rect(v, t.ox - c / 2.0, t.z1 + c / 2.0, ext.0, ext.1));
    }
    if v.sw.on(Sw::Water) {
        if let Some(l) = &v.water {
            paint::image(window, &l.img, rect(v, t.ox - c / 2.0, t.z1 + c / 2.0, ext.0, ext.1));
        }
    }
    if let Some((img, size)) = v.zone_layer().map(|(l, s)| (l.img.clone(), s)) {
        paint::image(window, &img, rect(v, t.ox - c / 2.0, t.z1 + c / 2.0, size, size));
    }
    if let Some((o, l)) = &v.overlay {
        paint::image(window, &l.img, rect(v, o.x0, o.z0 + o.size, o.size, o.size));
    }
    let sp = |v: &Viewer, x: f64, z: f64| p(ox + v.sx(x), oy + v.sy(z));
    let heli = v.is_heli();
    if heli {
        let bx0 = ox + v.sx(t.ox);
        let bz0 = oy + v.sy(t.z1);
        let bx1 = ox + v.sx(t.x1);
        let bz1 = oy + v.sy(t.oz);
        paint::stroke(window, &[p(bx0, bz0), p(bx1, bz0), p(bx1, bz1), p(bx0, bz1)], 1.2, color(0xffffff, 0.22), Some(&[6.0, 5.0]), true);
    }
    if v.sw.on(Sw::Grid) && v.view.s > 0.005 {
        let step = if v.view.s > 0.4 { 100.0 } else if v.view.s > 0.05 { 500.0 } else { 2000.0 };
        let (x0, x1) = ((v.view.cx - w as f64 / 2.0 / v.view.s) / step, (v.view.cx + w as f64 / 2.0 / v.view.s) / step);
        let (z0, z1) = ((v.view.cz - h as f64 / 2.0 / v.view.s) / step, (v.view.cz + h as f64 / 2.0 / v.view.s) / step);
        let g = color(0xffffff, 0.08);
        for k in x0.ceil() as i64..=x1.floor() as i64 {
            let x = ox + v.sx(k as f64 * step);
            paint::stroke(window, &[p(x, oy), p(x, oy + h)], 1.0, g, None, false);
        }
        for k in z0.ceil() as i64..=z1.floor() as i64 {
            let y = oy + v.sy(k as f64 * step);
            paint::stroke(window, &[p(ox, y), p(ox + w, y)], 1.0, g, None, false);
        }
    }
    if v.sw.on(Sw::Caps) || heli {
        let areas = v.areas();
        for a in &areas {
            if a.kind == "battle" || a.kind == "kill" {
                continue; // those two are in the zone layer
            }
            let col = color(scene::area_color(a), 1.0);
            match scene::area_shape(a) {
                Shape::Poly(q) => {
                    let pts: Vec<_> = q.iter().map(|q| sp(v, q[0], q[1])).collect();
                    if a.kind == "capture" {
                        paint::fill(window, &pts, color(0xf2d14b, 0.15));
                    }
                    paint::stroke(window, &pts, 1.5, col, None, true);
                }
                Shape::Circle { c, r } => {
                    let pts = paint::circle_pts(sp(v, c[0], c[1]), ((r * v.view.s) as f32).max(3.0));
                    if a.kind == "capture" {
                        paint::fill(window, &pts, color(0xf2d14b, 0.15));
                    }
                    paint::stroke(window, &pts, 1.5, col, None, true);
                }
            }
        }
        for (lbl, cx_, cz_) in scene::capture_zones_labeled(&areas) {
            let short = lbl.chars().last().unwrap_or('A').to_string();
            paint::badge(window, cx, sp(v, cx_, cz_), &short, color(0xd8b236, 0.92), color(0x111111, 1.0));
        }
        for pd in &m.meta.helipads {
            let hc = if pd.team == 1 { 0x4fb3e8 } else { 0xf09648 };
            let (ux, uz) = pd.basis();
            let (hx, hz) = pd.extents();
            let wpt = |lx: f64, lz: f64| sp(v, pd.x + lx * ux - lz * uz, pd.z + lx * uz + lz * ux);
            if hx * v.view.s >= 9.0 {
                let apron = [wpt(-hx, -hz), wpt(hx, -hz), wpt(hx, hz), wpt(-hx, hz)];
                paint::fill(window, &apron, color(0x888e88, 0.16));
                paint::stroke(window, &apron, 1.2, color(hc, 0.52), None, true);
            }
            if hx * v.view.s >= 26.0 {
                let pf = v.helipad_lib.as_ref().and_then(|lib| lib.prefabs.get(&pd.uc));
                let rwy = pf.map_or((hx - 14.0).max(80.0), |p| p.rwy as f64);
                let strip = [wpt(-rwy, -13.5), wpt(rwy, -13.5), wpt(rwy, 13.5), wpt(-rwy, 13.5)];
                paint::fill(window, &strip, color(0x23272d, 0.78));
                paint::stroke(window, &[wpt(-rwy + 6.0, 0.0), wpt(rwy - 6.0, 0.0)], 1.0, color(0xeef2f6, 0.60), Some(&[5.0, 5.0]), false);
                if let Some(pf) = pf {
                    for &[sx_, sz_] in &pf.spawns {
                        let (sx_, sz_) = (sx_ as f64, sz_ as f64);
                        if sx_.hypot(sz_) > 16.0 {
                            let h_sq = [wpt(sx_ - 7.2, sz_ - 7.2), wpt(sx_ + 7.2, sz_ - 7.2), wpt(sx_ + 7.2, sz_ + 7.2), wpt(sx_ - 7.2, sz_ + 7.2)];
                            paint::fill(window, &h_sq, color(0x3a3f46, 0.85));
                            paint::stroke(window, &h_sq, 1.0, color(0xeec43e, 0.82), None, true);
                        }
                    }
                }
            }
            let c = sp(v, pd.x, pd.z);
            let r = 8.0f32;
            let sq = [point(c.x - px(r), c.y - px(r)), point(c.x + px(r), c.y - px(r)), point(c.x + px(r), c.y + px(r)), point(c.x - px(r), c.y + px(r))];
            paint::fill(window, &sq, color(0x0c0e10, 0.88));
            paint::stroke(window, &sq, 2.0, color(hc, 1.0), None, true);
            let yr = pd.yaw * crate::core::DEG;
            let tip = p(f32::from(c.x) + yr.sin() as f32 * 20.0, f32::from(c.y) - yr.cos() as f32 * 20.0);
            paint::stroke(window, &[c, tip], 2.0, color(hc, 1.0), None, false);
            paint::text(window, cx, "H", point(c.x - px(4.5), c.y - px(7.5)), color(hc, 1.0), 11.0, true);
            paint::text(window, cx, &format!("{} ({:.0}m)", pd.short, pd.y), point(c.x + px(11.), c.y - px(8.)), color(hc, 1.0), 11.0, true);
        }
    }
    if heli {
        if let Some(a) = v.a {
            let ca = sp(v, a.x, a.z);
            for km in [2000.0, 4000.0, 6000.0] {
                let rad = (km * v.view.s) as f32;
                if rad > 18.0 && rad < w.max(h) * 1.5 {
                    paint::stroke(window, &paint::circle_pts(ca, rad), 1.0, color(0x52b6ec, 0.25), Some(&[4.0, 5.0]), true);
                    paint::text(window, cx, &format!("{}km", (km / 1000.0) as i32), point(ca.x + px(4.), ca.y - px(rad + 14.)), color(0x8bc9ec, 0.75), 10.5, false);
                }
            }
            if let Some(max_s) = v.heli_max_s {
                let rad = (max_s * v.view.s) as f32;
                if rad > 18.0 && rad < w.max(h) * 2.0 {
                    paint::stroke(window, &paint::circle_pts(ca, rad), 1.6, color(0xf09648, 0.75), Some(&[7.0, 5.0]), true);
                    paint::text(window, cx, &format!("极限射程 {:.2}km", max_s / 1000.0), point(ca.x + px(6.), ca.y + px(rad - 16.)), color(0xf09648, 0.92), 11.0, true);
                }
            }
        }
    }
    if let (Some(a), Some(bb)) = (v.a, v.b) {
        let clear = v.aim.as_ref().is_some_and(|q| q.los.is_none());
        paint::stroke(window, &[sp(v, a.x, a.z), sp(v, bb.x, bb.z)], 1.5, color(if clear { 0x58c472 } else { 0xe2574c }, 1.0), Some(&[4.0, 4.0]), false);
        if let Some(aim) = &v.aim {
            for s in &aim.sols {
                if let Some(bl) = s.blocked {
                    let f = bl / aim.d;
                    paint::dot(window, sp(v, a.x + (bb.x - a.x) * f, a.z + (bb.z - a.z) * f), 4.0, Some(color(0xe2574c, 1.0)), None);
                }
            }
        }
    }
    if v.sw.on(Sw::Red) {
        // the red zones' numbers from "Choose red zones"; zones switched off keep a grey badge to find them again
        for (i, (_, _, (x, z), on)) in v.red_zones().into_iter().enumerate() {
            let (fill, ink) = if on { (color(0xb01818, 0.95), color(0xffffff, 1.0)) } else { (color(0x3a3f45, 0.85), color(0xb8c0c8, 1.0)) };
            paint::badge(window, cx, sp(v, x, z), &(i + 1).to_string(), fill, ink);
        }
    }
    let a_lbl = if heli { format!("A (+{:.0}m)", v.params_pub(cx).gun_h) } else { "A".into() };
    for (pt, label, col) in [(v.a, a_lbl.as_str(), COL_A), (v.b, "B", COL_BPT)] {
        if let Some(q) = pt {
            let c = sp(v, q.x, q.z);
            paint::dot(window, c, 6.0, Some(color(col, 1.0)), Some((color(0, 1.0), 2.0)));
            paint::text(window, cx, label, point(c.x + px(9.), c.y - px(20.)), color(0xffffff, 1.0), 12.0, true);
        }
    }
    if let Some((x, z, hh)) = v.high_shown() {
        let c = sp(v, x, z);
        let tri = [point(c.x, c.y - px(9.)), point(c.x + px(8.), c.y + px(6.)), point(c.x - px(8.), c.y + px(6.))];
        paint::fill(window, &tri, color(COL_HIGH, 1.0));
        paint::stroke(window, &tri, 2.0, color(0, 1.0), None, true);
        paint::text(window, cx, &format!("最高点 {hh:.1} m"), point(c.x + px(11.), c.y - px(18.)), color(0xffffff, 1.0), 12.0, true);
    }
}

/// Side view along A -> B: ground, objects and crowns, sight line, trajectories.
pub fn profile(v: &mut Viewer, b: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    window.paint_quad(gpui::fill(b, color(0x101316, 1.0)));
    let muted = color(0x8a939c, 1.0);
    let (Some(aim), Some(m), Some(a)) = (&v.aim, &v.map, v.a) else {
        paint::text(window, cx, "弹道高低剖面图：在地图上左键放置开火点 A、右键放置目标点 B 后，此处显示沿线地形与弹道轨迹。", p(ox + 12.0, oy + 8.0), muted, 12.0, false);
        return;
    };
    let t = &m.t;
    let (d, dir) = (aim.d, aim.dir);
    let l = d * 1.08;
    let n = 1200.min((l / (t.cell / 2.0)).ceil() as usize).max(2);
    let samp = |tt: &crate::core::Terrain| (0..=n).map(|i| { let s = l * i as f64 / n as f64; (s, m.surf_at(tt, a.x + dir.0 * s, a.z + dir.1 * s)) }).collect::<Vec<_>>();
    let (prof, prof_r, prof_v) = (samp(&m.t), samp(&m.tr), samp(&m.tv));
    let mut lo = prof.iter().map(|q| q.1).fold(f64::MAX, f64::min).min(aim.y_a).min(aim.y_b);
    let mut hi = prof_v.iter().map(|q| q.1).fold(f64::MIN, f64::max).max(aim.y_a).max(aim.y_b);
    for q in &aim.sols {
        for i in 0..q.tr.s.len() {
            if q.tr.s[i] <= l {
                hi = hi.max(q.tr.y[i]);
            }
        }
    }
    let pad = ((hi - lo) * 0.1).max(2.0);
    lo -= pad;
    hi += pad;
    let mg = 34.0;
    let x = |s: f64| ox + mg + (s / l) as f32 * (w - 2.0 * mg);
    let y = |yy: f64| oy + h - 18.0 - ((yy - lo) / (hi - lo)) as f32 * (h - 36.0);
    for (pts, col) in [(&prof_v, 0x24452a), (&prof_r, 0x6b6259), (&prof, 0x3b4a35)] {
        let mut poly = vec![p(x(0.0), oy + h)];
        poly.extend(pts.iter().map(|&(s, yy)| p(x(s), y(yy))));
        poly.push(p(x(l), oy + h));
        paint::fill(window, &poly, color(col, 1.0));
    }
    paint::stroke(window, &[p(x(0.0), y(aim.y_a)), p(x(d), y(aim.y_b))], 1.0, color(if aim.los.is_none() { 0x58c472 } else { 0xe2574c }, 1.0), Some(&[5.0, 4.0]), false);
    for (i, q) in aim.sols.iter().enumerate() {
        let mut pts: Vec<_> = (0..q.tr.s.len()).take_while(|&k| q.tr.s[k] <= d).map(|k| p(x(q.tr.s[k]), y(q.tr.y[k]))).collect();
        pts.push(p(x(d), y(q.tr.at(Key::Y, d))));
        paint::stroke(window, &pts, 2.0, color(if i > 0 { COL_B } else { COL_A }, 1.0), None, false);
    }
    for (s, yy, c, tx) in [(0.0, aim.y_a, COL_A, "A"), (d, aim.y_b, COL_BPT, "B")] {
        paint::dot(window, p(x(s), y(yy)), 4.0, Some(color(c, 1.0)), None);
        paint::text(window, cx, tx, p(x(s) + 6.0, y(yy) - 18.0), color(c, 1.0), 12.0, false);
    }
    paint::text(window, cx, &format!("{lo:.0} m"), p(ox + 2.0, oy + h - 32.0), muted, 12.0, false);
    paint::text(window, cx, &format!("{hi:.0} m"), p(ox + 2.0, oy + 2.0), muted, 12.0, false);
    paint::text(window, cx, "0", p(x(0.0), oy + h - 16.0), muted, 12.0, false);
    paint::text(window, cx, &format!("{d:.0} m"), p(x(d) - 20.0, oy + h - 16.0), muted, 12.0, false);
    paint::text(window, cx, "纵向高度比例已放大显示", p(ox + w - 150.0, oy + 2.0), muted, 12.0, false);
}
