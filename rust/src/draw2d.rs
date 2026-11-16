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
    if v.sw.on(Sw::Grid) && v.view.s > 0.05 {
        let step = if v.view.s > 0.4 { 100.0 } else { 500.0 };
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
    if v.sw.on(Sw::Caps) {
        for a in v.areas() {
            if a.kind == "battle" || a.kind == "kill" {
                continue; // those two are in the zone layer
            }
            let col = color(scene::area_color(&a), 1.0);
            match scene::area_shape(&a) {
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
    for (pt, label, col) in [(v.a, "A", COL_A), (v.b, "B", COL_BPT)] {
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
    let samp = |tt: &crate::core::Terrain| (0..=n).map(|i| { let s = l * i as f64 / n as f64; (s, tt.height_at(a.x + dir.0 * s, a.z + dir.1 * s)) }).collect::<Vec<_>>();
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
