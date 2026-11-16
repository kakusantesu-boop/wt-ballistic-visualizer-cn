#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release builds
//! WT Ballistic Visualizer: GPUI (UI) and wgpu (3D views).
//! Reads the same ../data files, or the data bundled into the executable (src/bin/bundle.rs).
//! Run: cargo run --release

mod app;
mod core;
mod data;
mod draw2d;
mod paint;
mod raster;
mod scene;
mod store;
mod view3d;

use gpui::*;
use gpui_component::{Root, theme::{Theme, ThemeMode}};
use gpui_platform::application;

fn main() {
    application().with_assets(gpui_kit_assets::Assets).run(|cx: &mut App| { // icons of the widgets (check marks, dropdown arrows)
        gpui_component::set_locale("zh-CN");
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).font_family = "Microsoft YaHei UI".into();
        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("婴儿弹道计算机".into()), ..Default::default() }),
            ..Default::default()
        };
        cx.open_window(opts, |window, cx| {
            let v = cx.new(|cx| app::Viewer::new(window, cx));
            cx.new(|cx| Root::new(v, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
