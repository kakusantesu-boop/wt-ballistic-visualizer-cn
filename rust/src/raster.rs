//! CPU images shared by the 2D map and the 3D terrain: relief shading, ground colours, water and zones.
//! All are straight-alpha RGBA with +Z drawn upward (row 0 = the map's top edge), like the page's canvases.

use image::RgbaImage;
use tiny_skia::{BlendMode, Color, FillRule, Paint, PathBuilder, Pattern, Pixmap, Rect, SpreadMode, Stroke, Transform};

use crate::core::Terrain;
use crate::data::LoadedMap;
use crate::scene::{Shape, Zones};

const LIGHT: [f32; 3] = [-0.55, 0.75, 0.37];

/// Relief shading, one pixel per cell. grey: a multiplier for the ground colours (flat ground = 1), else a height tint.
pub fn shade(t: &Terrain, grey: bool) -> RgbaImage {
    let (w, h, hv, cell) = (t.w, t.hh, &t.h, t.cell as f32);
    let span = ((t.hi - t.lo) as f32).max(1.0);
    let mut img = RgbaImage::new(w as u32, h as u32);
    let px: &mut [u8] = &mut img;
    for j in 0..h {
        let r = (h - 1 - j) * w * 4;
        for i in 0..w {
            let c = hv[j * w + i];
            let gx = (hv[j * w + (i + 1).min(w - 1)] - hv[j * w + i.saturating_sub(1)]) / (2.0 * cell);
            let gz = (hv[(j + 1).min(h - 1) * w + i] - hv[j.saturating_sub(1) * w + i]) / (2.0 * cell);
            let nl = (gx * gx + 1.0 + gz * gz).sqrt();
            let lit = ((-gx * LIGHT[0] + LIGHT[1] - gz * LIGHT[2]) / nl).max(0.0);
            let o = r + i * 4;
            px[o + 3] = 255;
            if grey {
                let v = (255.0 * (0.3 + 0.7 * lit / 0.75).min(1.0)) as u8;
                px[o..o + 3].fill(v);
                continue;
            }
            let tt = (c - t.lo as f32) / span;
            let k = 0.35 + 0.75 * lit;
            let (rr, gg, bb) = if c < 0.0 {
                (40.0, 70.0, 100.0)
            } else if tt < 0.5 {
                (70.0 + 120.0 * tt, 95.0 + 70.0 * tt, 60.0 + 50.0 * tt)
            } else {
                (130.0 + 110.0 * (tt - 0.5), 130.0 + 100.0 * (tt - 0.5), 85.0 + 120.0 * (tt - 0.5))
            };
            px[o] = (rr * k).min(255.0) as u8;
            px[o + 1] = (gg * k).min(255.0) as u8;
            px[o + 2] = (bb * k).min(255.0) as u8;
        }
    }
    img
}

/// The map image: the game's ground colours multiplied by the relief of ground + roofs, else a height tint.
pub fn compose(map: &LoadedMap) -> RgbaImage {
    let Some(g) = &map.ground else { return shade(&map.tr, false) };
    let s = image::imageops::resize(&shade(&map.tr, true), g.color.width(), g.color.height(), image::imageops::FilterType::Triangle);
    let mut out = g.color.clone();
    for (o, m) in out.pixels_mut().zip(s.pixels()) {
        for c in 0..3 {
            o.0[c] = (o.0[c] as u16 * m.0[c] as u16 / 255) as u8;
        }
        o.0[3] = 255;
    }
    out
}

/// Outer airspace satellite colour map multiplied by LandMesh hillshade relief.
pub fn compose_air(t_air: &Terrain, color: Option<&RgbaImage>) -> RgbaImage {
    let Some(col) = color else { return shade(t_air, false) };
    let s = if col.width() == t_air.w as u32 && col.height() == t_air.hh as u32 {
        shade(t_air, true)
    } else {
        image::imageops::resize(&shade(t_air, true), col.width(), col.height(), image::imageops::FilterType::Triangle)
    };
    let mut out = col.clone();
    for (o, m) in out.pixels_mut().zip(s.pixels()) {
        for c in 0..3 {
            o.0[c] = (o.0[c] as u16 * m.0[c] as u16 / 255) as u8;
        }
        o.0[3] = 255;
    }
    out
}

/// Water over the bare ground below the level's water plane; deeper is darker and more opaque.
pub fn water(t: &Terrain, level: f64) -> RgbaImage {
    let (w, h) = (t.w, t.hh);
    let mut img = RgbaImage::new(w as u32, h as u32);
    let px: &mut [u8] = &mut img;
    for j in 0..h {
        let r = (h - 1 - j) * w;
        for i in 0..w {
            let d = level as f32 - t.h[j * w + i];
            if d <= 0.0 {
                continue;
            }
            let k = (d / 12.0).min(1.0);
            let o = (r + i) * 4;
            px[o] = (72.0 - 50.0 * k) as u8;
            px[o + 1] = (142.0 - 78.0 * k) as u8;
            px[o + 2] = (178.0 - 60.0 * k) as u8;
            px[o + 3] = (255.0 * (0.55 + 0.37 * (d / 4.0).min(1.0))) as u8;
        }
    }
    img
}

/// Zone layer over the square map extent `size` m from (T.ox - cell/2, T.z1 + cell/2) down-right.
pub struct ZoneLayer {
    pub img: RgbaImage,
    pub size: f64,
}

/// Map border: stripes inside the map bounds but outside every battle area, red border at the battle area's edge
/// (none without a battle area). Red zones: the kill areas, solid dark red.
pub fn zones(map: &LoadedMap, z: &Zones, border: bool, red: bool) -> ZoneLayer {
    let t = &map.t;
    let c = t.cell;
    let (x0, z1) = (t.ox - c / 2.0, t.z1 + c / 2.0);
    let ext = t.w.max(t.hh) as f64 * c;
    let n = 2048.min(t.w.max(t.hh)) as u32;
    let k = n as f64 / ext;
    let mut pm = Pixmap::new(n, n).expect("zone layer size");
    let tf = |x: f64, zz: f64| (((x - x0) * k) as f32, ((z1 - zz) * k) as f32);
    let path = |sh: &Shape| {
        let mut pb = PathBuilder::new();
        match sh {
            Shape::Poly(p) => {
                for (i, q) in p.iter().enumerate() {
                    let (a, b) = tf(q[0], q[1]);
                    if i == 0 { pb.move_to(a, b) } else { pb.line_to(a, b) }
                }
                pb.close();
            }
            Shape::Circle { c, r } => {
                let (a, b) = tf(c[0], c[1]);
                pb.push_circle(a, b, ((r * k) as f32).max(0.01));
            }
        }
        pb.finish()
    };
    if border && !z.battle.is_empty() {
        let [bx0, bz0, bx1, bz1] = map.meta.bounds.unwrap_or([x0, z1 - ext, x0 + ext, z1]);
        let mut tile = Pixmap::new(16, 16).unwrap();
        tile.fill(Color::from_rgba8(230, 20, 20, 56));
        let mut sp = Paint::default();
        sp.set_color_rgba8(255, 25, 25, 217);
        sp.anti_alias = true;
        let mut pb = PathBuilder::new();
        for (a, b, cc, d) in [(-4.0, 20.0, 20.0, -4.0), (-4.0, 4.0, 4.0, -4.0), (12.0, 20.0, 20.0, 12.0)] {
            pb.move_to(a, b);
            pb.line_to(cc, d);
        }
        tile.stroke_path(&pb.finish().unwrap(), &sp, &Stroke { width: 3.0, ..Default::default() }, Transform::identity(), None);
        let mut paint = Paint::default();
        paint.shader = Pattern::new(tile.as_ref(), SpreadMode::Repeat, tiny_skia::FilterQuality::Nearest, 1.0, Transform::identity());
        let (a, b) = tf(bx0, bz1);
        if let Some(r) = Rect::from_xywh(a, b, ((bx1 - bx0) * k) as f32, ((bz1 - bz0) * k) as f32) {
            pm.fill_rect(r, &paint, Transform::identity(), None);
        }
        let mut erase = Paint { blend_mode: BlendMode::DestinationOut, ..Default::default() };
        erase.set_color_rgba8(0, 0, 0, 255);
        erase.anti_alias = true;
        for s in &z.battle {
            if let Some(p) = path(s) {
                pm.fill_path(&p, &erase, FillRule::Winding, Transform::identity(), None);
            }
        }
        let mut line = Paint { anti_alias: true, ..Default::default() };
        line.set_color_rgba8(255, 30, 30, 255);
        let st = Stroke { width: (4.0 * n as f32 / 2048.0).max(2.0), ..Default::default() };
        for s in &z.battle {
            if let Some(p) = path(s) {
                pm.stroke_path(&p, &line, &st, Transform::identity(), None);
            }
        }
    }
    if red && !z.kill.is_empty() {
        // one light tint over the union of all red zones (overlaps would stack to near-opaque red and hide the map),
        // then every zone's own edge: a dark halo under a bright line, readable on any ground
        let mut union = Pixmap::new(n, n).expect("zone layer size");
        let mut fill = Paint { anti_alias: true, ..Default::default() };
        fill.set_color_rgba8(170, 10, 10, 255);
        let paths: Vec<_> = z.kill.iter().filter_map(path).collect();
        for p in &paths {
            union.fill_path(p, &fill, FillRule::Winding, Transform::identity(), None);
        }
        pm.draw_pixmap(0, 0, union.as_ref(), &tiny_skia::PixmapPaint { opacity: 0.3, ..Default::default() }, Transform::identity(), None);
        let w = (3.0 * n as f32 / 2048.0).max(2.0);
        for (rgba, width) in [([40, 0, 0, 190], w + 2.5), ([255, 70, 60, 255], w)] {
            let mut line = Paint { anti_alias: true, ..Default::default() };
            line.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
            for p in &paths {
                pm.stroke_path(p, &line, &Stroke { width, ..Default::default() }, Transform::identity(), None);
            }
        }
    }
    ZoneLayer { img: demultiply(pm), size: ext }
}

fn demultiply(pm: Pixmap) -> RgbaImage {
    let (w, h) = (pm.width(), pm.height());
    let data = pm.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    RgbaImage::from_raw(w, h, data.collect()).expect("pixmap size")
}
