//! Canvas helpers on top of GPUI's painting: the 2D map, the profile and the 3D overlay draw with these.

use std::sync::Arc;

use gpui::{App, Bounds, Corners, Hsla, PathBuilder, Pixels, Point, RenderImage, TextAlign, Window, point, px, rgb};

pub fn color(c: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(c).into();
    h.a = a;
    h
}

pub fn p(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

/// Polyline (closed: back to the start). dash: on/off lengths in px.
pub fn stroke(window: &mut Window, pts: &[Point<Pixels>], width: f32, col: Hsla, dash: Option<&[f32]>, closed: bool) {
    if pts.len() < 2 {
        return;
    }
    let mut b = PathBuilder::stroke(px(width));
    if let Some(d) = dash {
        b = b.dash_array(&d.iter().map(|&v| px(v)).collect::<Vec<_>>());
    }
    b.move_to(pts[0]);
    for q in &pts[1..] {
        b.line_to(*q);
    }
    if closed {
        b.close();
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, col);
    }
}

pub fn fill(window: &mut Window, pts: &[Point<Pixels>], col: Hsla) {
    if pts.len() < 3 {
        return;
    }
    let mut b = PathBuilder::fill();
    b.move_to(pts[0]);
    for q in &pts[1..] {
        b.line_to(*q);
    }
    b.close();
    if let Ok(path) = b.build() {
        window.paint_path(path, col);
    }
}

pub fn circle_pts(c: Point<Pixels>, r: f32) -> Vec<Point<Pixels>> {
    let n = ((r * 1.5) as usize).clamp(12, 64);
    (0..n).map(|i| {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        point(c.x + px(r * t.cos()), c.y + px(r * t.sin()))
    }).collect()
}

/// Filled circle with an optional outline.
pub fn dot(window: &mut Window, c: Point<Pixels>, r: f32, fill_col: Option<Hsla>, line: Option<(Hsla, f32)>) {
    let pts = circle_pts(c, r);
    if let Some(f) = fill_col {
        fill(window, &pts, f);
    }
    if let Some((col, w)) = line {
        stroke(window, &pts, w, col, None, true);
    }
}

/// One line of text with its top-left at `at`; returns its width.
pub fn text(window: &mut Window, cx: &mut App, s: &str, at: Point<Pixels>, col: Hsla, size: f32, bold: bool) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let mut run = window.text_style().to_run(s.len());
    run.color = col;
    if bold {
        run.font.weight = gpui::FontWeight::BOLD;
    }
    let line = window.text_system().shape_line(s.to_string().into(), px(size), &[run], None);
    let w = f32::from(line.width);
    line.paint(at, px(size * 1.25), TextAlign::Left, None, window, cx).ok();
    w
}

/// Monospace text (aim readouts in the 3D view).
pub fn mono(window: &mut Window, cx: &mut App, s: &str, at: Point<Pixels>, col: Hsla, size: f32) {
    let mut run = window.text_style().to_run(s.len());
    run.color = col;
    run.font.family = "Microsoft YaHei UI".into();
    let line = window.text_system().shape_line(s.to_string().into(), px(size), &[run], None);
    line.paint(at, px(size * 1.25), TextAlign::Left, None, window, cx).ok();
}

/// Label on a dark box, anchored up-right of a point (the page's lbl()).
pub fn label_box(window: &mut Window, cx: &mut App, at: Point<Pixels>, s: &str, col: Hsla) {
    let mut run = window.text_style().to_run(s.len());
    run.color = col;
    run.font.weight = gpui::FontWeight::BOLD;
    let line = window.text_system().shape_line(s.to_string().into(), px(11.), &[run], None);
    let w = f32::from(line.width);
    let b = Bounds::new(point(at.x + px(8.), at.y - px(22.)), gpui::size(px(w + 10.), px(17.)));
    window.paint_quad(gpui::fill(b, color(0x0a0c0e, 0.78)));
    line.paint(point(at.x + px(13.), at.y - px(20.)), px(14.), TextAlign::Left, None, window, cx).ok();
}

/// A number in a round badge centred on `c` (red zone labels).
pub fn badge(window: &mut Window, cx: &mut App, c: Point<Pixels>, s: &str, fill_col: Hsla, text_col: Hsla) {
    let mut run = window.text_style().to_run(s.len());
    run.color = text_col;
    run.font.weight = gpui::FontWeight::BOLD;
    let line = window.text_system().shape_line(s.to_string().into(), px(11.), &[run], None);
    let w = f32::from(line.width);
    dot(window, c, (w / 2.0 + 5.0).max(9.0), Some(fill_col), Some((color(0xffffff, 0.9), 1.5)));
    line.paint(point(c.x - px(w / 2.0), c.y - px(7.)), px(14.), TextAlign::Left, None, window, cx).ok();
}

/// RGBA (straight alpha) to the BGRA image GPUI draws.
pub fn render_image(img: &image::RgbaImage) -> Arc<RenderImage> {
    let mut bgra = img.clone();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(bgra)]))
}

pub fn image(window: &mut Window, img: &Arc<RenderImage>, rect: Bounds<Pixels>) {
    window.paint_image(rect, rect, Corners::default(), img.clone(), 0, false).ok();
}
