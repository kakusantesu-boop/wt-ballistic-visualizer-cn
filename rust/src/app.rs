//! The window: sidebar (map, layers, gun, aim, sweeps, controls), the map (2D or 3D) and the side profile.
//! State changes recompute what depends on them (aim, zones, 3D frame) and notify the view.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    IndexPath, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    searchable_list::{SearchableListItem, SearchableVec},
    select::{Select, SelectEvent, SelectState},
    switch::Switch,
};

use crate::core::{Pt, Shell};
use crate::data::{self, Index, LoadedMap, TankMesh};
use crate::paint::{self, color};
use crate::raster;
use crate::scene::{self, Aim, GameMode, Overlay, Params, Perf, SweepKind, Zones};
use crate::view3d::{self, View3D};

pub const COL_A: u32 = 0xe0a33a;
pub const COL_B: u32 = 0xe98a3c;
pub const COL_BPT: u32 = 0x4fb3e8;
pub const COL_HIGH: u32 = 0xc77dff;
pub const TARGET_MODEL: &str = "t_72a";

/// One row of a dropdown: shown title, extra search text, value.
#[derive(Clone)]
pub struct Item {
    title: SharedString,
    search: String,
    value: usize,
    note: Option<SharedString>, // shown right-aligned in the list (map size)
}
impl SearchableListItem for Item {
    type Value = usize;
    fn title(&self) -> SharedString {
        self.title.clone()
    }
    fn value(&self) -> &usize {
        &self.value
    }
    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().w_full().flex().justify_between().gap_2().child(self.title.clone()).when_some(self.note.clone(), |d, n| d.child(div().text_xs().opacity(0.6).child(n)))
    }
    fn matches(&self, q: &str) -> bool {
        let q = q.trim().to_lowercase();
        let q_ns = q.replace(' ', "");
        self.title.to_lowercase().contains(&q)
            || self.search.contains(&q)
            || self.search.contains(&q_ns)
            || self.search.contains(&q.replace(' ', "_"))
    }
}
fn items<S: Into<SharedString>>(titles: impl IntoIterator<Item = S>) -> Vec<Item> {
    titles.into_iter().enumerate().map(|(value, t)| Item { title: t.into(), search: String::new(), value, note: None }).collect()
}
type Sel = Entity<SelectState<SearchableVec<Item>>>; // SearchableVec: a plain Vec does not filter on search

/// Level names without the avg_ prefix, words capitalised: avg_red_desert -> Red Desert
pub fn map_title(name: &str) -> String {
    name.trim_start_matches("avg_").split('_').map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Map2D,
    Orbit,
    Aim,
    /// Gunner's sight: the aim camera at a magnification, with a reticle and range marks
    Sight,
}

/// Layer and option switches.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sw {
    Border,
    Red,
    Caps,
    Water,
    Grid,
    Build,
    Trees,
    Tanks,
    GroundDetail,
    VegSight,
    VegShots,
    Arc,
    Clip,
}

pub struct Switches(std::collections::HashSet<Sw>);
impl Switches {
    pub fn on(&self, s: Sw) -> bool {
        self.0.contains(&s)
    }
}

pub struct View2D {
    pub cx: f64,
    pub cz: f64,
    pub s: f64,
}

struct Drag {
    start: Point<Pixels>,
    last: Point<Pixels>,
    btn: MouseButton,
    pan: bool,
    moved: bool,
    view: (f64, f64),
}

/// Images of the current map for both views (RGBA for the 3D textures, GPUI images for 2D).
pub struct Layer {
    pub rgba: Arc<image::RgbaImage>,
    pub img: Arc<RenderImage>,
}
impl Layer {
    fn new(rgba: image::RgbaImage) -> Self {
        Self { img: paint::render_image(&rgba), rgba: Arc::new(rgba) }
    }
}

pub struct Viewer {
    store: Arc<data::Store>,
    pub index: Arc<Index>,
    pub helipad_lib: Option<Arc<data::HelipadLib>>,
    focus: FocusHandle,
    map_sel: Sel,
    tank_sel: Sel,
    mission_sel: Sel,
    mode_sel: Sel,
    view_sel: Sel,
    shell_sel: Sel,
    detail_sel: Sel,
    perf_sel: Sel,
    pmin: Entity<InputState>,
    pmax: Entity<InputState>,
    range: Entity<InputState>,
    gun_h: Entity<InputState>,
    tgt_h: Entity<InputState>,
    heli_agl_in: Entity<InputState>,
    heli_spd_in: Entity<InputState>,
    pub heli_agl: f64,
    pub heli_spd: f64,
    veh_filter: &'static str,
    pub sw: Switches,

    pub map: Option<Arc<LoadedMap>>,
    map_gen: u64,
    pub map_img: Option<Layer>,
    pub water: Option<Layer>,
    pub air_img: Option<Layer>,
    pub air_water: Option<Layer>,
    pub zones_img: Option<(String, Layer, f64)>,
    pub overlay: Option<(Overlay, Layer)>,
    pub heli_lines: Vec<(String, u8)>,
    pub heli_max_s: Option<f64>,
    pub mission: usize,
    pub mode: GameMode,
    pub view_mode: ViewMode,
    pub detail: usize,
    pub perf: Perf,
    pub tank: usize,
    pub gun_shell: (usize, usize),
    pub a: Option<Pt>,
    pub b: Option<Pt>,
    /// A's hull heading once driven (azimuth: 0 = +Z, clockwise to +X); None = facing B
    pub heading: Option<f64>,
    /// red zones (kill areas, by name) switched off in the list; reset per map
    hidden_red: std::collections::HashSet<String>,
    red_open: bool,
    drive_keys: std::collections::HashSet<&'static str>,
    driving: bool,
    pub aim: Option<Aim>,
    pub high: Option<(String, f64, f64, f64)>,
    pub view: View2D,
    map_bounds: Bounds<Pixels>,
    drag: Option<Drag>,
    hud: String,
    status: (String, bool),
    sweeping: Option<Task<()>>,
    pending_missions: Option<(Vec<Item>, usize)>,
    pending_view: Option<usize>,
    center_pending: bool,
    pub tanks: std::collections::HashMap<String, Option<Arc<TankMesh>>>,
    pub v3: View3D,
}

impl Viewer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = Arc::new(data::Store::open());
        let (index, load_err) = match data::load_index(&store) {
            Ok(i) => (Arc::new(i), None),
            Err(e) => (Arc::new(Index { maps: vec![], tanks: vec![] }), Some(format!("未找到地图数据（{}）：{e:#}。请运行单文件打包版，或将 data 文件夹放到程序同目录下。", store.describe()))),
        };
        let last = Last::load();
        let map_items: Vec<Item> = index.maps.iter().enumerate().map(|(i, m)| {
            let title = m.title.clone().unwrap_or_else(|| map_title(&m.name));
            let search = format!("{} {} {} {}", m.name, map_title(&m.name), m.en.as_deref().unwrap_or(""), title.replace(' ', "")).to_lowercase();
            Item { title: title.into(), search, value: i, note: Some(format!("{:.1} km", m.width as f64 * m.cell / 1000.0).into()) }
        }).collect();
        let tank_items: Vec<Item> = index.tanks.iter().enumerate().map(|(i, t)| {
            let search = format!("{} {} {}", t.id, t.en.as_deref().unwrap_or(""), t.name.replace(' ', "")).to_lowercase();
            Item { title: t.name.clone().into(), search, value: i, note: None }
        }).collect();
        let map_i = last.map.as_ref().and_then(|n| index.maps.iter().position(|m| &m.name == n)).unwrap_or(0);
        let tank_i = last.tank.as_ref().and_then(|n| index.tanks.iter().position(|t| &t.id == n)).unwrap_or(0);
        let sel = |items: Vec<Item>, i: usize, window: &mut Window, cx: &mut Context<Self>| -> Sel {
            let has = !items.is_empty();
            cx.new(|cx| SelectState::new(SearchableVec::new(items), has.then(|| IndexPath::new(i)), window, cx).searchable(true))
        };
        let map_sel = sel(map_items, map_i, window, cx);
        let tank_sel = sel(tank_items, tank_i, window, cx);
        let mission_sel = sel(vec![], 0, window, cx);
        let mode_sel = sel(items(GameMode::ALL.map(|m| m.label())), 1, window, cx);
        let view_sel = sel(items(["2D 俯视地图", "3D 自由环绕", "3D 炮口/机头视角（A → B）", "3D 瞄准镜视角"]), 0, window, cx);
        let shell_sel = sel(vec![], 0, window, cx);
        let detail_sel = sel(items(view3d::DETAIL_LABELS), 2, window, cx);
        let perf_sel = sel(items(Perf::ALL.map(|p| p.label())), 1, window, cx);
        let num = |v: &str, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).default_value(v.to_string()));
        let (pmin, pmax, range, gun_h, tgt_h) = (num("-10", window, cx), num("20", window, cx), num("2500", window, cx), num("2.0", window, cx), num("1.5", window, cx));
        let (heli_agl_in, heli_spd_in) = (num("35", window, cx), num("0", window, cx));

        for s in [&map_sel, &tank_sel, &mission_sel, &mode_sel, &view_sel, &shell_sel, &detail_sel, &perf_sel] {
            let which = s.entity_id();
            cx.subscribe_in(s, window, move |this, _, ev: &SelectEvent<SearchableVec<Item>>, window, cx| {
                let SelectEvent::Confirm(Some(v)) = ev else { return };
                this.on_select(which, *v, window, cx);
            }).detach();
        }
        for i in [&pmin, &pmax, &range, &gun_h, &tgt_h, &heli_agl_in, &heli_spd_in] {
            cx.subscribe_in(i, window, |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.update_aim(cx);
                }
            }).detach();
        }
        let helipad_lib = data::load_helipads(&store).ok().map(Arc::new);
        let sw = Switches([Sw::Border, Sw::Red, Sw::Caps, Sw::Water, Sw::Grid, Sw::Build, Sw::Trees, Sw::Tanks, Sw::GroundDetail, Sw::VegSight, Sw::Arc, Sw::Clip].into_iter().collect());
        let mut this = Self {
            store, index, helipad_lib, focus: cx.focus_handle(), map_sel, tank_sel, mission_sel, mode_sel, view_sel, shell_sel, detail_sel, perf_sel,
            pmin, pmax, range, gun_h, tgt_h, heli_agl_in, heli_spd_in, heli_agl: 35.0, heli_spd: 0.0, veh_filter: "all", sw,
            map: None, map_gen: 0, map_img: None, water: None, air_img: None, air_water: None, zones_img: None, overlay: None, heli_lines: vec![], heli_max_s: None,
            mission: 0, mode: GameMode::Realistic, view_mode: ViewMode::Map2D, detail: 2, perf: Perf::Balanced,
            tank: tank_i, gun_shell: (0, 0), a: None, b: None, heading: None, hidden_red: Default::default(), red_open: false, drive_keys: Default::default(), driving: false, aim: None, high: None,
            view: View2D { cx: 0.0, cz: 0.0, s: 0.25 }, map_bounds: Bounds::default(), drag: None, hud: String::new(),
            status: (String::new(), false), sweeping: None, pending_missions: None, pending_view: None, center_pending: false, tanks: Default::default(), v3: View3D::default(),
        };
        if let Some(e) = load_err {
            this.set_status(e, true);
        } else if this.index.maps.is_empty() {
            this.set_status("data/index.js 中无地图数据：请先运行 extractor/extract.py", true);
        } else {
            this.pick_tank(tank_i, last.shell, window, cx);
            this.load_tank(TARGET_MODEL, cx);
            this.load_map(map_i, cx);
        }
        this
    }

    fn set_status(&mut self, s: impl Into<String>, bad: bool) {
        self.status = (s.into(), bad);
    }

    pub fn is_heli(&self) -> bool {
        self.index.tanks.get(self.tank).is_some_and(|t| t.is_heli())
    }

    pub fn heli_v_air(&self) -> f64 {
        if self.is_heli() { self.heli_spd.max(0.0) / 3.6 } else { 0.0 }
    }

    pub fn shell(&self) -> Option<&Shell> {
        let t = self.index.tanks.get(self.tank)?;
        t.guns.get(self.gun_shell.0)?.shells.get(self.gun_shell.1)
    }

    pub fn params_pub(&self, cx: &App) -> Params {
        self.params(cx)
    }

    fn params(&self, cx: &App) -> Params {
        let f = |e: &Entity<InputState>, d: f64| e.read(cx).value().trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(d);
        let heli = self.is_heli();
        let agl = f(&self.heli_agl_in, 35.0).clamp(0.0, 4000.0);
        let spd = f(&self.heli_spd_in, 0.0).clamp(0.0, 600.0);
        Params {
            p_min: f(&self.pmin, if heli { -35.0 } else { -10.0 }),
            p_max: f(&self.pmax, if heli { 45.0 } else { 20.0 }),
            range: f(&self.range, if heli { 6500.0 } else { 2500.0 }).max(100.0),
            gun_h: if heli { agl } else { f(&self.gun_h, 2.0) },
            tgt_h: f(&self.tgt_h, 1.5),
            v_air: if heli { spd / 3.6 } else { 0.0 },
        }
    }

    /// The mission's areas for the game mode, without the red zones switched off in the list.
    pub fn areas(&self) -> Vec<data::Area> {
        let mut a = self.all_areas();
        a.retain(|a| !(a.kind == "kill" && self.hidden_red.contains(&a.name)));
        a
    }
    fn all_areas(&self) -> Vec<data::Area> {
        self.map.as_ref().and_then(|m| m.meta.missions.get(self.mission)).map(|m| scene::mission_areas(m, self.mode)).unwrap_or_default()
    }
    fn set_red(&mut self, names: impl IntoIterator<Item = String>, on: bool, cx: &mut Context<Self>) {
        for n in names {
            if on { self.hidden_red.remove(&n); } else { self.hidden_red.insert(n); }
        }
        self.v3.dirty();
        cx.notify();
    }
    fn zones(&self) -> Zones {
        Zones::of(&self.areas())
    }
    /// Surface shots collide with / sight is blocked by.
    pub fn tc(&self) -> Option<&crate::core::Terrain> {
        self.map.as_ref().map(|m| if self.sw.on(Sw::VegShots) { &m.tv } else { &m.tr })
    }
    fn ts(&self) -> Option<&crate::core::Terrain> {
        self.map.as_ref().map(|m| if self.sw.on(Sw::VegSight) { &m.tv } else { &m.tr })
    }
    /// Map, mission, mode and the red zones switched off: what the zones and the highest point depend on.
    fn high_key(&self) -> String {
        let mut off: Vec<&str> = self.hidden_red.iter().map(String::as_str).collect();
        off.sort();
        format!("{}|{}|{:?}|{}", self.map.as_ref().map_or("", |m| m.meta.name.as_str()), self.mission, self.mode, off.join(","))
    }
    pub fn map_bounds(&self) -> Bounds<Pixels> {
        self.map_bounds
    }
    pub fn sw_bits(&self) -> u32 {
        self.sw.0.iter().map(|s| 1u32 << (*s as u32)).sum()
    }
    /// 3D aim view with A and B placed: the camera sits at A.
    pub fn aim_ready(&self) -> bool {
        matches!(self.view_mode, ViewMode::Aim | ViewMode::Sight) && self.aim.as_ref().is_some_and(|a| a.d >= 1.0)
    }
    pub fn is_dragging_3d(&self) -> bool {
        self.view_mode != ViewMode::Map2D && self.drag.as_ref().is_some_and(|d| d.moved)
    }
    pub fn high_shown(&self) -> Option<(f64, f64, f64)> {
        self.high.as_ref().filter(|h| h.0 == self.high_key()).map(|h| (h.1, h.2, h.3))
    }

    // ---------------------------------------------------------------- loading

    fn load_map(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.index.maps.get(i).cloned() else { return };
        let mtitle = entry.title.clone().unwrap_or_else(|| map_title(&entry.name));
        self.map_gen += 1;
        let gen_ = self.map_gen;
        self.set_status(format!("正在加载 {}…", mtitle), false);
        let dir = self.store.clone();
        cx.spawn(async move |this, cx| {
            let name = entry.name.clone();
            let base = cx.background_spawn({
                let dir = dir.clone();
                async move {
                    let (meta, h16) = data::load_map(&dir, &name)?;
                    let map = LoadedMap::new(meta, h16);
                    let img = raster::compose(&map);
                    anyhow::Ok((map, img))
                }
            }).await;
            let map = match base {
                Ok((map, img)) => {
                    let map = Arc::new(map);
                    if this.update(cx, |this, cx| this.map_ready(gen_, map.clone(), img, cx)).ok() != Some(true) {
                        return;
                    }
                    map
                }
                Err(e) => {
                    this.update(cx, |this, cx| { this.set_status(format!("地图加载失败：{e:#}"), true); cx.notify(); }).ok();
                    return;
                }
            };
            if !entry.objects && !entry.ground && map.meta.air.is_none() {
                return;
            }
            this.update(cx, |this, cx| { this.set_status(format!("正在加载 {} 的全域外圈地形、建筑掩体与地表贴图…", mtitle), false); cx.notify(); }).ok();
            let full = cx.background_spawn(async move {
                let mut m = (*map).clone();
                let mut notes = vec![];
                if entry.objects {
                    match data::load_objects(&dir, &entry.name).and_then(|o| m.set_objects(o)) {
                        Ok(()) => {}
                        Err(e) => notes.push(format!("掩体模型加载失败：{e:#}")),
                    }
                }
                if entry.ground {
                    match data::load_ground(&dir, &entry.name) {
                        Ok(g) => m.ground = Some(Arc::new(g)),
                        Err(e) => notes.push(format!("地表贴图加载失败：{e:#}")),
                    }
                }
                let mut air_img = None;
                let mut air_water = None;
                if let Some(air_meta) = m.meta.air.clone() {
                    match data::load_air(&dir, &air_meta) {
                        Ok((t_air, air_col)) => {
                            air_img = Some(raster::compose_air(&t_air, Some(&air_col)));
                            air_water = m.meta.water.map(|lvl| raster::water(&t_air, lvl));
                            m.t_air = Some(Arc::new(t_air));
                        }
                        Err(e) => notes.push(format!("外圈空域地形加载失败：{e:#}")),
                    }
                }
                let img = raster::compose(&m);
                (m, img, air_img, air_water, notes)
            }).await;
            this.update(cx, |this, cx| {
                if this.map_gen != gen_ {
                    return;
                }
                let (m, img, air_img, air_water, notes) = full;
                let air_note = m.t_air.as_ref().map_or(String::new(), |ta| format!("，外圈 {:.1} km 空域地形已就绪", ta.w as f64 * ta.cell / 1000.0));
                let msg = match (&m.obj, notes.first()) {
                    (_, Some(n)) => (n.clone(), true),
                    (Some(o), None) => (format!("已加载 {} 处建筑/岩石，{} 棵林木{air_note}", o.inst.len(), o.trees.len()), false),
                    (None, None) => (air_note.trim_start_matches('，').to_string(), false),
                };
                this.map = Some(Arc::new(m));
                this.map_img = Some(Layer::new(img));
                this.air_img = air_img.map(Layer::new);
                this.air_water = air_water.map(Layer::new);
                this.status = msg;
                this.v3.map_changed();
                if let Ok(mode) = std::env::var("WTV_DEMO") {
                    this.demo(&mode);
                }
                this.update_aim(cx);
                match std::env::var("WTV_SWEEP").as_deref() {
                    Ok("reach") => this.run_sweep(SweepKind::Reach, cx),
                    Ok("hit") => this.run_sweep(SweepKind::Hit, cx),
                    Ok("view") => this.run_sweep(SweepKind::View, cx),
                    _ => {}
                }
            }).ok();
        }).detach();
    }

    /// The bare map arrived: reset per-map state. false when another map was picked meanwhile.
    fn map_ready(&mut self, gen_: u64, map: Arc<LoadedMap>, img: image::RgbaImage, cx: &mut Context<Self>) -> bool {
        if gen_ != self.map_gen {
            return false;
        }
        self.mission = map.meta.missions.iter().position(|m| m.name.ends_with("_dom")).unwrap_or(0);
        let gen_items: Vec<Item> = if map.meta.missions.is_empty() {
            items(["（无任务配置）"])
        } else {
            map.meta.missions.iter().enumerate().map(|(i, m)| {
                let title = m.title.clone().unwrap_or_else(|| m.name.clone());
                let search = format!("{} {}", m.name, title.replace(' ', "")).to_lowercase();
                Item { title: title.into(), search, value: i, note: None }
            }).collect()
        };
        let mission = self.mission;
        self.water = map.meta.water.map(|lvl| Layer::new(raster::water(&map.t, lvl)));
        self.air_img = None;
        self.air_water = None;
        let auto_pad = if self.is_heli() { map.meta.helipads.first().cloned() } else { None };
        self.map = Some(map);
        self.map_img = Some(Layer::new(img));
        self.zones_img = None;
        self.a = auto_pad.as_ref().map(|p| Pt { x: p.x, z: p.z });
        self.b = None;
        self.heading = auto_pad.as_ref().map(|p| p.yaw * crate::core::DEG);
        self.aim = None;
        self.overlay = None;
        self.high = None;
        self.hidden_red.clear();
        self.v3.map_changed();
        self.pending_missions = Some((gen_items, mission)); // applied in render, which has the window
        self.set_status("", false);
        self.center_pending = self.map_bounds.size.width <= px(0.);
        if self.is_heli() {
            self.fit_airspace();
        } else {
            self.center_map();
        }
        self.remember(cx);
        self.update_aim(cx);
        true
    }

    /// WTV_DEMO=2d|orbit|orbitA|orbitB|aim|sight (WTV_SWEEP=reach|hit|view): A and B 1.2 km apart across a capture zone, highest point found (for screenshots).
    fn demo(&mut self, mode: &str) {
        let Some(map) = self.map.clone() else { return };
        let areas = self.areas();
        let (cx_, cz) = areas.iter().find(|a| a.kind == "capture").map_or(((map.t.ox + map.t.x1) / 2.0, (map.t.oz + map.t.z1) / 2.0), |a| (a.tm[3][0], a.tm[3][2]));
        self.a = Some(Pt { x: cx_ - 600.0, z: cz - 200.0 });
        self.b = Some(Pt { x: cx_ + 600.0, z: cz + 200.0 });
        if let Some((x, z, h)) = scene::highest_point(&map, &self.zones()) {
            self.high = Some((self.high_key(), x, z, h));
        }
        self.view_mode = match mode { "2d" => ViewMode::Map2D, "aim" => ViewMode::Aim, "sight" => ViewMode::Sight, _ => ViewMode::Orbit };
        self.pending_view = Some(self.view_mode as usize);
        match mode {
            "orbitA" => self.v3.fit(&map, cx_ - 600.0, cz - 200.0, 22.0),
            "orbitB" => self.v3.fit(&map, cx_ + 600.0, cz + 200.0, 22.0),
            _ => self.v3.fit(&map, cx_, cz, 1800.0),
        }
    }

    pub fn load_tank(&mut self, model: &str, cx: &mut Context<Self>) {
        if model.is_empty() || self.tanks.contains_key(model) {
            return;
        }
        self.tanks.insert(model.to_string(), None);
        let (dir, model) = (self.store.clone(), model.to_string());
        cx.spawn(async move |this, cx| {
            let m = cx.background_spawn({ let model = model.clone(); async move { data::load_tank(&dir, &model) } }).await;
            if let Ok(m) = m {
                this.update(cx, |this, cx| { this.tanks.insert(model, Some(Arc::new(m))); cx.notify(); }).ok();
            } // no file: the point keeps only its marker
        }).detach();
    }

    fn set_veh_filter(&mut self, flt: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.veh_filter = flt;
        let filtered: Vec<Item> = self.index.tanks.iter().enumerate().filter(|(_, t)| match flt {
            "heli" => t.is_heli(),
            "ground" => !t.is_heli(),
            _ => true,
        }).map(|(i, t)| {
            let search = format!("{} {} {}", t.id, t.en.as_deref().unwrap_or(""), t.name.replace(' ', "")).to_lowercase();
            Item { title: t.name.clone().into(), search, value: i, note: None }
        }).collect();
        if filtered.is_empty() {
            return;
        }
        let sel_pos = filtered.iter().position(|it| it.value == self.tank);
        let pick_idx = sel_pos.unwrap_or(0);
        let target_tank = filtered[pick_idx].value;
        let ts = self.tank_sel.clone();
        ts.update(cx, |s, cx| {
            s.set_items(SearchableVec::new(filtered), window, cx);
            s.set_selected_index(Some(IndexPath::new(pick_idx)), window, cx);
        });
        if target_tank != self.tank {
            self.pick_tank(target_tank, None, window, cx);
        } else {
            cx.notify();
        }
    }

    fn snap_helipad(&mut self, pad_id: &str, cx: &mut Context<Self>) {
        let Some(map) = self.map.clone() else { return };
        let Some(pd) = map.meta.helipads.iter().find(|h| h.id == pad_id) else { return };
        self.a = Some(Pt { x: pd.x, z: pd.z });
        self.heading = Some(pd.yaw * crate::core::DEG);
        if self.b.is_none() {
            if let Some((_, cx_, cz_)) = scene::capture_zones_labeled(&self.areas()).first().cloned() {
                self.b = Some(Pt { x: cx_, z: cz_ });
            }
        }
        self.fit_airspace();
        self.update_aim(cx);
    }

    fn ensure_range_covers(&mut self, b: Pt, window: &mut Window, cx: &mut Context<Self>) {
        let Some(map) = self.map.as_ref() else { return };
        let origin = self.a.unwrap_or(Pt { x: (map.t.ox + map.t.x1) * 0.5, z: (map.t.oz + map.t.z1) * 0.5 });
        let dist = (b.x - origin.x).hypot(b.z - origin.z);
        let needed = ((dist + 800.0) / 500.0).ceil() * 500.0;
        let cur = self.params(cx).range;
        if cur < needed {
            let next = needed.clamp(2500.0, 20000.0);
            self.range.update(cx, |s, cx| s.set_value(format!("{:.0}", next), window, cx));
        }
    }

    fn frame_ground_and_point(&mut self, pt: Pt) {
        let Some(map) = self.map.clone() else { return };
        let t = &map.t;
        let (mut x0, mut z0, mut x1, mut z1) = (t.ox, t.oz, t.x1, t.z1);
        if let Some(b) = self.areas().iter().find(|a| a.kind == "battle") {
            [x0, z0, x1, z1] = scene::area_shape(b).bbox();
        }
        if let Some(a) = self.a {
            x0 = x0.min(a.x);
            x1 = x1.max(a.x);
            z0 = z0.min(a.z);
            z1 = z1.max(a.z);
        }
        x0 = x0.min(pt.x);
        x1 = x1.max(pt.x);
        z0 = z0.min(pt.z);
        z1 = z1.max(pt.z);
        let pad = 450.0f64.max(0.10 * (x1 - x0).max(z1 - z0));
        x0 -= pad;
        x1 += pad;
        z0 -= pad;
        z1 += pad;
        self.view.cx = (x0 + x1) * 0.5;
        self.view.cz = (z0 + z1) * 0.5;
        let (w, h) = (f64::from(f32::from(self.map_bounds.size.width)).max(400.0), f64::from(f32::from(self.map_bounds.size.height)).max(300.0));
        self.view.s = (0.90 * (w / (x1 - x0)).min(h / (z1 - z0))).clamp(0.004, 40.0);
        self.v3.fit(&map, self.view.cx, self.view.cz, (x1 - x0).max(z1 - z0));
    }

    fn target_helipad(&mut self, pad_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(map) = self.map.clone() else { return };
        let Some(pd) = map.meta.helipads.iter().find(|h| h.id == pad_id).cloned() else { return };
        let target = Pt { x: pd.x, z: pd.z };
        self.b = Some(target);
        self.ensure_range_covers(target, window, cx);
        self.frame_ground_and_point(target);
        self.set_status(format!("已设目标 B：{}（停机坪台面 {:.1} m）", pd.name, pd.y), false);
        self.update_aim(cx);
    }

    fn snap_2d_pad(&self, p: Pt, pos: Point<Pixels>) -> Pt {
        if self.view_mode != ViewMode::Map2D {
            return p;
        }
        let Some(map) = self.map.as_ref() else { return p };
        let (lx, ly) = self.local(pos);
        for pd in &map.meta.helipads {
            if self.view.s >= 0.22 {
                if let Some(pf) = self.helipad_lib.as_ref().and_then(|lib| lib.prefabs.get(&pd.uc)) {
                    let (ux, uz) = pd.basis();
                    for &[sx_l, sz_l] in &pf.spawns {
                        let (sx_l, sz_l) = (sx_l as f64, sz_l as f64);
                        let (wx_s, wz_s) = (pd.x + sx_l * ux - sz_l * uz, pd.z + sx_l * uz + sz_l * ux);
                        if (self.sx(wx_s) - lx).hypot(self.sy(wz_s) - ly) <= 12.0 {
                            return Pt { x: wx_s, z: wz_s };
                        }
                    }
                }
            }
            if (self.sx(pd.x) - lx).hypot(self.sy(pd.z) - ly) <= 16.0 {
                return Pt { x: pd.x, z: pd.z };
            }
        }
        p
    }

    fn fit_airspace(&mut self) {
        let Some(map) = self.map.clone() else { return };
        let t = &map.t;
        let (mut x0, mut z0, mut x1, mut z1) = (t.ox, t.oz, t.x1, t.z1);
        for pd in &map.meta.helipads {
            x0 = x0.min(pd.x);
            x1 = x1.max(pd.x);
            z0 = z0.min(pd.z);
            z1 = z1.max(pd.z);
        }
        let pad = 600.0;
        x0 -= pad;
        x1 += pad;
        z0 -= pad;
        z1 += pad;
        self.view.cx = (x0 + x1) * 0.5;
        self.view.cz = (z0 + z1) * 0.5;
        let (w, h) = (f64::from(f32::from(self.map_bounds.size.width)).max(400.0), f64::from(f32::from(self.map_bounds.size.height)).max(300.0));
        self.view.s = (0.90 * (w / (x1 - x0)).min(h / (z1 - z0))).clamp(0.004, 40.0);
        self.v3.fit(&map, self.view.cx, self.view.cz, (x1 - x0).max(z1 - z0));
    }

    fn pick_tank(&mut self, i: usize, shell_key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.index.tanks.get(i).cloned() else { return };
        let was_heli = self.is_heli();
        self.tank = i;
        let now_heli = t.is_heli();
        let mut keys = vec![];
        let mut shell_items = vec![];
        for (gi, g) in t.guns.iter().enumerate() {
            let gname = g.title.as_deref().unwrap_or(&g.weapon);
            for (si, s) in g.shells.iter().enumerate() {
                let idx_val = keys.len();
                keys.push((gi, si));
                let stype = s.type_title.as_deref().unwrap_or_else(|| s.kind.trim_end_matches("_tank"));
                let spd_str = match &s.rocket {
                    Some(rm) if rm.force0 > 0.0 && rm.t_fire0 > 0.0 => {
                        if rm.v_start > 1.0 {
                            format!("初速{:.0}→{:.0} m/s", rm.v_start, s.v0)
                        } else {
                            format!("助推{:.1}s→{:.0} m/s", rm.t_fire0 + rm.t_fire1, s.v0)
                        }
                    }
                    _ => format!("{} m/s", s.v0.round()),
                };
                let extra = format!(
                    "{}{}",
                    if s.rocket.as_ref().is_some_and(|r| r.guided) { " · 制导" } else { "" },
                    if s.cx_est { " · 估算风阻" } else { "" }
                );
                let title = format!(
                    "{}{} ({}) {spd_str}{extra}",
                    if t.guns.len() > 1 { format!("{gname}: ") } else { String::new() },
                    s.name,
                    stype,
                );
                let search = format!("{} {} {} {}", g.weapon, s.kind, s.en.as_deref().unwrap_or(""), title.replace(' ', "")).to_lowercase();
                shell_items.push(Item { title: title.into(), search, value: idx_val, note: None });
            }
        }
        let k = shell_key.and_then(|k| keys.iter().position(|q| format!("{}:{}", q.0, q.1) == k)).unwrap_or(0);
        self.gun_shell = keys.get(k).copied().unwrap_or((0, 0));
        let ss = self.shell_sel.clone();
        ss.update(cx, |s, cx| { s.set_items(SearchableVec::new(shell_items), window, cx); s.set_selected_index(Some(IndexPath::new(k)), window, cx); });
        if let Some(m) = &t.model {
            self.load_tank(m, cx);
        }
        if !was_heli && now_heli {
            self.range.update(cx, |s, cx| s.set_value("6500".to_string(), window, cx));
            if let Some(map) = self.map.clone() {
                if self.a.is_none() {
                    if let Some(pd) = map.meta.helipads.first() {
                        self.a = Some(Pt { x: pd.x, z: pd.z });
                        self.heading = Some(pd.yaw * crate::core::DEG);
                    }
                }
                self.fit_airspace();
            }
        } else if was_heli && !now_heli {
            self.range.update(cx, |s, cx| s.set_value("2500".to_string(), window, cx));
            if let Some(map) = self.map.clone() {
                if let Some(a) = self.a {
                    if !map.t.inside(a.x, a.z) {
                        self.a = None;
                    }
                }
                if let Some(b) = self.b.filter(|b| !map.t.inside(b.x, b.z)) {
                    self.ensure_range_covers(b, window, cx);
                    self.frame_ground_and_point(b);
                } else {
                    self.center_map();
                }
            }
        }
        self.pick_shell(k, window, cx);
    }

    fn pick_shell(&mut self, k: usize, window: &mut Window, cx: &mut Context<Self>) {
        let t = &self.index.tanks[self.tank];
        let keys: Vec<(usize, usize)> = t.guns.iter().enumerate().flat_map(|(gi, g)| (0..g.shells.len()).map(move |si| (gi, si))).collect();
        self.gun_shell = keys.get(k).copied().unwrap_or((0, 0));
        let mut pitch = t.guns.get(self.gun_shell.0).map_or([-10.0, 20.0], |g| g.pitch);
        if (pitch[1] - pitch[0]).abs() < 0.1 {
            pitch = [pitch[0] - 15.0, pitch[1] + 25.0];
        }
        self.pmin.update(cx, |s, cx| s.set_value(pitch[0].to_string(), window, cx));
        self.pmax.update(cx, |s, cx| s.set_value(pitch[1].to_string(), window, cx));
        self.remember(cx);
        self.update_aim(cx);
    }

    fn remember(&self, _cx: &App) {
        Last {
            map: self.map.as_ref().map(|m| m.meta.name.clone()),
            tank: self.index.tanks.get(self.tank).map(|t| t.id.clone()),
            shell: Some(format!("{}:{}", self.gun_shell.0, self.gun_shell.1)),
        }.save();
    }

    fn on_select(&mut self, which: EntityId, v: usize, window: &mut Window, cx: &mut Context<Self>) {
        if which == self.map_sel.entity_id() {
            if self.map.as_ref().map(|m| m.meta.name.as_str()) != self.index.maps.get(v).map(|m| m.name.as_str()) {
                self.load_map(v, cx);
            }
        } else if which == self.tank_sel.entity_id() {
            self.pick_tank(v, None, window, cx);
        } else if which == self.shell_sel.entity_id() {
            self.pick_shell(v, window, cx);
        } else if which == self.mission_sel.entity_id() {
            self.mission = v;
            self.update_aim(cx);
        } else if which == self.mode_sel.entity_id() {
            self.mode = GameMode::ALL[v];
            self.update_aim(cx);
        } else if which == self.view_sel.entity_id() {
            self.view_mode = [ViewMode::Map2D, ViewMode::Orbit, ViewMode::Aim, ViewMode::Sight][v];
        } else if which == self.detail_sel.entity_id() {
            self.detail = v;
        } else if which == self.perf_sel.entity_id() {
            self.perf = Perf::ALL[v];
        }
        cx.notify();
    }

    fn toggle(&mut self, s: Sw, on: bool, cx: &mut Context<Self>) {
        if on { self.sw.0.insert(s); } else { self.sw.0.remove(&s); }
        if matches!(s, Sw::VegShots | Sw::VegSight) {
            self.update_aim(cx);
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- derived state

    /// Aim A -> B and cached helicopter firing tables (recomputed only on state changes, never on mouse move/paint).
    pub fn update_aim(&mut self, cx: &mut Context<Self>) {
        self.aim = None;
        self.heli_lines.clear();
        self.heli_max_s = None;
        let p = self.params(cx);
        if let (Some(map), Some(sh), Some(a), Some(b), Some(tc), Some(ts)) = (self.map.as_ref(), self.shell(), self.a, self.b, self.tc(), self.ts()) {
            self.aim = Some(scene::compute_aim(map, tc, ts, sh, a, b, &p));
        }
        if self.is_heli() {
            self.heli_agl = p.gun_h;
            self.heli_spd = p.v_air * 3.6;
            if let (Some(map), Some(tc), Some(sh), Some(a)) = (self.map.clone(), self.tc().cloned(), self.shell().cloned(), self.a) {
                let areas = self.areas();
                let (lines, max_s) = scene::heli_aim_lines(&map, &tc, &areas, &sh, a, self.aim.as_ref(), &p);
                self.heli_lines = lines;
                self.heli_max_s = max_s;
            } else {
                self.heli_lines = vec![(if self.map.is_some() { "请点击上方【1队近H1 / 2队近H1】停机坪或在地图上左键放置直升机 A 点。" } else { "请先加载一张地图。" }.to_string(), 0)];
            }
        }
        self.v3.dirty();
        cx.notify();
    }

    /// Zone layer for the current map, mission, mode and switches (cached).
    pub fn zone_layer(&mut self) -> Option<(&Layer, f64)> {
        let map = self.map.clone()?;
        let key = format!("{}|{}", self.high_key(), u8::from(self.sw.on(Sw::Border)) * 2 + u8::from(self.sw.on(Sw::Red)));
        if self.zones_img.as_ref().is_none_or(|z| z.0 != key) {
            let z = raster::zones(&map, &self.zones(), self.sw.on(Sw::Border), self.sw.on(Sw::Red));
            self.zones_img = Some((key, Layer::new(z.img), z.size));
        }
        self.zones_img.as_ref().map(|z| (&z.1, z.2))
    }

    fn center_map(&mut self) {
        let Some(map) = self.map.clone() else { return };
        let t = &map.t;
        let (mut x0, mut z0, mut x1, mut z1) = (t.ox, t.oz, t.x1, t.z1);
        if let Some(b) = self.areas().iter().find(|a| a.kind == "battle") {
            [x0, z0, x1, z1] = scene::area_shape(b).bbox();
        }
        self.view.cx = (x0 + x1) / 2.0;
        self.view.cz = (z0 + z1) / 2.0;
        let (w, h) = (f64::from(f32::from(self.map_bounds.size.width)).max(400.0), f64::from(f32::from(self.map_bounds.size.height)).max(300.0));
        self.view.s = 0.92 * (w / (x1 - x0)).min(h / (z1 - z0));
        self.v3.fit(&map, self.view.cx, self.view.cz, (x1 - x0).max(z1 - z0));
    }

    fn find_high(&mut self, cx: &mut Context<Self>) {
        let Some(map) = self.map.clone() else { return };
        match scene::highest_point(&map, &self.zones()) {
            Some((x, z, h)) => {
                self.high = Some((self.high_key(), x, z, h));
                self.view.cx = x;
                self.view.cz = z;
                self.v3.fit(&map, x, z, 900.0);
            }
            None => self.set_status("未找到有效可用地面", true),
        }
        self.v3.dirty();
        cx.notify();
    }

    fn run_sweep(&mut self, kind: SweepKind, cx: &mut Context<Self>) {
        let (Some(map), Some(shell)) = (self.map.clone(), self.shell().cloned()) else { return };
        let pt = if kind == SweepKind::Hit { self.b } else { self.a };
        let Some(pt) = pt else {
            self.set_status(if kind == SweepKind::Hit { "请先放置目标点 B（鼠标右键点击地图）。" } else { "请先放置开火点 A（鼠标左键点击地图）。" }, true);
            cx.notify();
            return;
        };
        let p = self.params(cx);
        let perf = self.perf;
        let veg = self.sw.on(if kind == SweepKind::View { Sw::VegSight } else { Sw::VegShots });
        // Reach: targets may stand in red zones (each team's apply to the other team), so only the battle area clips.
        // Hit B: shooter positions, which red zones rule out.
        let zones = self.sw.on(Sw::Clip).then(|| {
            let mut z = self.zones();
            if kind == SweepKind::Reach {
                z.kill.clear();
            }
            z
        });
        let label = match kind { SweepKind::Reach => "A 点火力覆盖", SweepKind::Hit => "可打击 B 点的阵地", SweepKind::View => "A 点视野通视" };
        self.set_status(format!("{label}：正在使用 {} 线程并行解算…", rayon::current_num_threads()), false);
        let t0 = std::time::Instant::now();
        self.sweeping = Some(cx.spawn(async move |this, cx| {
            let ov = cx.background_spawn(async move {
                let t = if veg { &map.tv } else { &map.tr };
                scene::run_sweep(kind, &map, t, &shell, pt, &p, perf, zones.as_ref())
            }).await;
            this.update(cx, |this, cx| {
                let what = match kind { SweepKind::Reach => "火力覆盖面积", SweepKind::Hit => "可打击阵地面积", SweepKind::View => "可见区域面积" };
                this.set_status(format!("{what}：{:.2} km²（耗时 {:.1} s）", ov.km2, t0.elapsed().as_secs_f64()), false);
                let layer = Layer::new(ov.img.clone());
                this.overlay = Some((ov, layer));
                this.sweeping = None;
                this.v3.dirty();
                cx.notify();
            }).ok();
        }));
        cx.notify();
    }

    // ---------------------------------------------------------------- map input

    pub fn sx(&self, x: f64) -> f32 {
        ((x - self.view.cx) * self.view.s) as f32 + f32::from(self.map_bounds.size.width) / 2.0
    }
    pub fn sy(&self, z: f64) -> f32 {
        f32::from(self.map_bounds.size.height) / 2.0 - ((z - self.view.cz) * self.view.s) as f32
    }
    fn wx(&self, px_: f32) -> f64 {
        (px_ - f32::from(self.map_bounds.size.width) / 2.0) as f64 / self.view.s + self.view.cx
    }
    fn wz(&self, py: f32) -> f64 {
        (f32::from(self.map_bounds.size.height) / 2.0 - py) as f64 / self.view.s + self.view.cz
    }
    fn local(&self, p: Point<Pixels>) -> (f32, f32) {
        (f32::from(p.x - self.map_bounds.origin.x), f32::from(p.y - self.map_bounds.origin.y))
    }
    /// World point under a window position, in the current view.
    fn world_at(&mut self, pos: Point<Pixels>) -> Option<Pt> {
        let (lx, ly) = self.local(pos);
        let map = self.map.clone()?;
        if self.view_mode == ViewMode::Map2D {
            Some(Pt { x: self.wx(lx), z: self.wz(ly) })
        } else {
            self.v3.pick(&map, lx, ly, self.is_heli())
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if ev.click_count == 2 && ev.button == MouseButton::Left && self.view_mode != ViewMode::Map2D {
            if let (Some(p), Some(map)) = (self.world_at(ev.position), self.map.clone()) {
                if !self.aim_ready() {
                    self.v3.center_on(&map, p);
                    cx.notify();
                }
            }
            return;
        }
        self.drag = Some(Drag { start: ev.position, last: ev.position, btn: ev.button, pan: ev.button != MouseButton::Left || ev.modifiers.shift, moved: false, view: (self.view.cx, self.view.cz) });
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let actively_dragging = self.drag.as_ref().is_some_and(|d| d.moved);
        if !actively_dragging {
            if let (Some(map), true) = (self.map.clone(), self.map_bounds.contains(&ev.position)) {
                let next_hud = match self.world_at(ev.position) {
                    Some(p) => format!("X {:.0}  Z {:.0}  地面 {:.1} m{}", p.x, p.z, map.stand(p.x, p.z), self.a.map_or(String::new(), |a| format!("   距 A {:.0} m", (p.x - a.x).hypot(p.z - a.z)))),
                    None => String::new(),
                };
                if next_hud != self.hud {
                    self.hud = next_hud;
                    if self.drag.is_none() {
                        cx.notify();
                    }
                }
            }
        }
        let Some(d) = self.drag.as_mut() else { return };
        if ev.pressed_button.is_none() {
            self.drag = None;
            return;
        }
        let (dx, dy) = (f32::from(ev.position.x - d.start.x), f32::from(ev.position.y - d.start.y));
        if !d.moved && dx.abs() + dy.abs() <= 4.0 {
            return;
        }
        d.moved = true;
        if self.view_mode == ViewMode::Map2D {
            self.view.cx = d.view.0 - dx as f64 / self.view.s;
            self.view.cz = d.view.1 + dy as f64 / self.view.s;
        } else {
            let (sx, sy) = (f32::from(ev.position.x - d.last.x), f32::from(ev.position.y - d.last.y));
            d.last = ev.position;
            let pan = d.pan;
            let aim_ready = self.aim_ready();
            let heli = self.is_heli();
            if let (Some(map), false) = (self.map.clone(), aim_ready) {
                self.v3.drag(&map, sx, sy, pan, f32::from(self.map_bounds.size.height), heli);
            }
        }
        cx.notify();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        if d.moved {
            if self.view_mode != ViewMode::Map2D {
                cx.notify();
            }
            return;
        }
        if !self.map_bounds.contains(&ev.position) {
            return;
        }
        let Some(raw_p) = self.world_at(ev.position) else { return };
        let heli = self.is_heli();
        match d.btn {
            MouseButton::Left => {
                let p = if heli { self.snap_2d_pad(raw_p, ev.position) } else { raw_p };
                if !self.map.as_ref().is_some_and(|m| if heli { p.x.abs() <= 30000.0 && p.z.abs() <= 30000.0 } else { m.t.inside(p.x, p.z) }) {
                    return;
                }
                self.a = Some(p);
                self.heading = None; // a newly placed tank faces B
            }
            MouseButton::Right => {
                let p = self.snap_2d_pad(raw_p, ev.position);
                if !(p.x.abs() <= 30000.0 && p.z.abs() <= 30000.0) {
                    return;
                }
                self.b = Some(p);
                if !heli && self.map.as_ref().is_some_and(|m| !m.t.inside(p.x, p.z)) {
                    self.ensure_range_covers(p, window, cx);
                }
            }
            _ => return,
        }
        self.update_aim(cx);
    }

    fn wheel(&mut self, ev: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = -f32::from(ev.delta.pixel_delta(px(40.)).y); // the page's deltaY: positive = away from the user
        if self.view_mode == ViewMode::Map2D {
            let (lx, ly) = self.local(ev.position);
            let (x, z) = (self.wx(lx), self.wz(ly));
            self.view.s = (self.view.s * (-dy as f64 * 0.0015).exp()).clamp(0.004, 40.0);
            self.view.cx = x - (lx - f32::from(self.map_bounds.size.width) / 2.0) as f64 / self.view.s;
            self.view.cz = z + (ly - f32::from(self.map_bounds.size.height) / 2.0) as f64 / self.view.s;
        } else {
            let aim = self.aim_ready();
            self.v3.zoom(dy, aim, self.view_mode == ViewMode::Sight);
        }
        cx.notify();
    }

    /// W/S (or up/down) drive A's tank/heli, A/D steer it, Q/E climb/descend heli.
    fn drive_key(key: &str) -> Option<&'static str> {
        match key {
            "w" | "up" => Some("w"),
            "s" | "down" => Some("s"),
            "a" | "left" => Some("a"),
            "d" | "right" => Some("d"),
            "q" => Some("q"),
            "e" => Some("e"),
            _ => None,
        }
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some(k) = Self::drive_key(&ev.keystroke.key) {
            self.drive_keys.remove(k);
        }
    }

    /// One step of driving: move A along its heading and turn it, then update the aim. false = stop the loop.
    fn drive_step(&mut self, dt: f64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        const TURN: f64 = 45.0 * crate::core::DEG; // per second
        if !self.focus.is_focused(window) {
            self.drive_keys.clear(); // a key released elsewhere never arrives here
        }
        let (Some(map), Some(a)) = (self.map.clone(), self.a) else { self.drive_keys.clear(); self.driving = false; return false };
        if self.drive_keys.is_empty() {
            self.driving = false;
            return false;
        }
        let heli = self.is_heli();
        let on = |k: &str| f64::from(u8::from(self.drive_keys.contains(k)));
        let mut hd = self.heading.unwrap_or_else(|| self.b.map_or(0.0, |b| (b.x - a.x).atan2(b.z - a.z)));
        hd += (on("d") - on("a")) * TURN * dt;
        let (fwd, rev) = if heli { (60.0, 40.0) } else { (12.0, 5.0) };
        let speed = on("w") * fwd - on("s") * rev;
        let (x, z) = (a.x + hd.sin() * speed * dt, a.z + hd.cos() * speed * dt);
        if if heli { x.abs() <= 25000.0 && z.abs() <= 25000.0 } else { map.t.inside(x, z) } {
            self.a = Some(Pt { x, z });
        }
        if heli && (on("e") != 0.0 || on("q") != 0.0) {
            let next_agl = (self.heli_agl + (on("e") - on("q")) * 25.0 * dt).clamp(0.0, 3000.0);
            self.heli_agl = next_agl;
            self.heli_agl_in.update(cx, |s, cx| s.set_value(format!("{:.0}", next_agl), window, cx));
        }
        self.heading = Some(hd);
        self.update_aim(cx);
        true
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        if let Some(k) = Self::drive_key(&ev.keystroke.key) {
            if self.a.is_none() {
                self.set_status("请先左键点击地图放置战车 A，随后可用 W A S D 驾驶移动。", true);
                cx.notify();
                return;
            }
            self.drive_keys.insert(k);
            if !self.driving {
                self.driving = true;
                cx.spawn_in(window, async move |this, cx| {
                    let mut last = std::time::Instant::now();
                    loop {
                        cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                        let dt = last.elapsed().as_secs_f64().min(0.1);
                        last = std::time::Instant::now();
                        if !this.update_in(cx, |this, window, cx| this.drive_step(dt, window, cx)).unwrap_or(false) {
                            break;
                        }
                    }
                }).detach();
            }
            return;
        }
        match ev.keystroke.key.as_str() {
            "r" => {
                let on = !self.sw.on(Sw::Border);
                self.toggle(Sw::Border, on, cx);
            }
            "/" => self.map_sel.update(cx, |s, cx| s.focus(window, cx)),
            _ => {}
        }
    }
}


/// Last map, tank and shell, kept between runs (the page's localStorage "wtv").
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Last {
    map: Option<String>,
    tank: Option<String>,
    shell: Option<String>,
}
impl Last {
    fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("wtv-viewer.json"))
    }
    fn load() -> Self {
        Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    fn save(&self) {
        if let (Some(p), Ok(b)) = (Self::path(), serde_json::to_vec(self)) {
            std::fs::write(p, b).ok(); // best effort, like the page's storage
        }
    }
}

// -------------------------------------------------------------------- rendering

const BG: u32 = 0x0c0e10;
const PANEL: u32 = 0x14181c;
const CARD: u32 = 0x1a1f24;
const BORDER: u32 = 0x262d34;
const MUTED: u32 = 0x8a939c;

fn card(title: &str) -> Div {
    div().flex().flex_col().gap_1p5().p_3().rounded_lg().bg(rgb(CARD)).border_1().border_color(rgb(BORDER)).child(
        div().flex().items_center().gap_1p5().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(COL_A)).child(div().size_1p5().rounded_full().bg(rgb(COL_A))).child(title.to_uppercase()),
    )
}
fn field(label: &str, el: impl IntoElement) -> Div {
    div().flex().flex_col().gap_0p5().flex_1().min_w_0().child(div().text_xs().text_color(rgb(MUTED)).child(label.to_string())).child(el)
}

impl Viewer {
    fn switch(&self, id: &'static str, s: Sw, title: &str, sub: Option<&str>, swatch: Option<u32>, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().items_center().gap_2().w_full().child(
            Switch::new(id).small().checked(self.sw.on(s)).on_click(cx.listener(move |this, v: &bool, _, cx| this.toggle(s, *v, cx))),
        ).when_some(swatch, |d, c| d.child(div().size_3().rounded_sm().bg(rgb(c))))
        .child(div().flex().flex_col().flex_1().min_w_0().child(div().text_sm().child(title.to_string())).when_some(sub, |d, s| d.child(div().text_xs().text_color(rgb(MUTED)).child(s.to_string()))))
    }

    /// The red zones of the selected mission and mode, numbered from 1 in list order: (name, team, label spot, on).
    /// The label spot is the zone's centre, pulled inside the map when the zone reaches beyond it.
    pub fn red_zones(&self) -> Vec<(String, Option<i64>, (f64, f64), bool)> {
        let mut zones: Vec<(String, Option<i64>, (f64, f64), bool)> = vec![];
        let Some(map) = &self.map else { return zones };
        let t = &map.t;
        let m = 40.0; // keep labels off the very edge
        for a in self.all_areas() {
            if a.kind == "kill" && !zones.iter().any(|z| z.0 == a.name) {
                let (x, z) = match scene::area_shape(&a) {
                    scene::Shape::Poly(p) => (p.iter().map(|q| q[0]).sum::<f64>() / 4.0, p.iter().map(|q| q[1]).sum::<f64>() / 4.0),
                    scene::Shape::Circle { c, .. } => (c[0], c[1]),
                };
                let on = !self.hidden_red.contains(&a.name);
                zones.push((a.name, a.team, (x.clamp(t.ox + m, t.x1 - m), z.clamp(t.oz + m, t.z1 - m)), on));
            }
        }
        zones
    }

    /// Fold-out list of the red zones of the selected mission and mode, each with its own switch.
    fn red_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let zones: Vec<(String, Option<i64>)> = self.red_zones().into_iter().map(|z| (z.0, z.1)).collect();
        let shown = zones.iter().filter(|z| !self.hidden_red.contains(&z.0)).count();
        let open = self.red_open;
        let names: Vec<String> = zones.iter().map(|z| z.0.clone()).collect();
        let (on_all, off_all) = (names.clone(), names);
        div().flex().flex_col().gap_1().pl(px(44.)).when(!zones.is_empty(), |d| {
            d.child(
                div().id("red-list").flex().items_center().gap_1().cursor_pointer().text_xs().text_color(rgb(MUTED))
                    .child(format!("{} 选择禁区红区（已开启 {shown}/{}）", if open { "▾" } else { "▸" }, zones.len()))
                    .on_click(cx.listener(|this, _, _, cx| { this.red_open = !this.red_open; cx.notify(); })),
            )
            .when(open, |d| {
                d.child(div().flex().gap_2()
                    .child(Button::new("red-all").label("全部开启").xsmall().on_click(cx.listener(move |this, _, _, cx| this.set_red(on_all.clone(), true, cx))))
                    .child(Button::new("red-none").label("全部关闭").xsmall().on_click(cx.listener(move |this, _, _, cx| this.set_red(off_all.clone(), false, cx)))))
                .children(zones.into_iter().enumerate().map(|(i, (name, team))| {
                    let label = name.replace('_', " ");
                    let who = match team { Some(1) => "1 队禁区", Some(2) => "2 队禁区", _ => "双方禁区" };
                    let n = name.clone();
                    Checkbox::new(("red-zone", i)).checked(!self.hidden_red.contains(&name)).label(format!("{}.  {label} · {who}", i + 1))
                        .on_click(cx.listener(move |this, on: &bool, _, cx| this.set_red([n.clone()], *on, cx)))
                }))
            })
        })
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let high_txt = self.high_shown().map(|(x, z, h)| format!("{h:.1} m（X {x:.0}, Z {z:.0}）")).unwrap_or_default();
        let heli = self.is_heli();
        let p_cur = self.params(cx);
        let aim_lines: Vec<(String, u8)> = if heli {
            if self.heli_lines.is_empty() {
                vec![(if self.map.is_some() { "请点击上方【1队近H1 / 2队近H1】停机坪或在地图上左键放置直升机 A 点。" } else { "请先加载一张地图。" }.to_string(), 0)]
            } else {
                self.heli_lines.clone()
            }
        } else {
            match &self.aim {
                Some(a) => {
                    let mut lines = scene::aim_lines(a, &p_cur);
                    if let (Some(m), Some(b)) = (self.map.as_ref(), self.b) {
                        if let Some(pd) = scene::pad_at(m, b) {
                            lines.insert(0, (format!("目标点位  {}（停机坪台面 {:.1} m）", pd.name, pd.y), 1));
                        }
                    }
                    lines
                }
                None => {
                    if let (Some(m), Some(b)) = (self.map.as_ref(), self.b) {
                        if let Some(pd) = scene::pad_at(m, b) {
                            vec![
                                (format!("已设目标 B：{}（台面 {:.1} m）", pd.name, pd.y), 1),
                                ("请左键点击陆战地图放置开火点 A，即可解算反制诸元。".into(), 0),
                            ]
                        } else {
                            vec![("已放置目标点 B，请左键点击陆战地图放置开火点 A。".into(), 0)]
                        }
                    } else {
                        vec![(if self.map.is_some() { "左键放置开火点 A，右键放置目标点 B（支持直升机停机坪）。" } else { "请先加载一张地图。" }.to_string(), 0)]
                    }
                }
            }
        };
        let n_all = self.index.tanks.len();
        let n_heli = self.index.tanks.iter().filter(|t| t.is_heli()).count();
        let n_ground = n_all.saturating_sub(n_heli);
        let flt = self.veh_filter;
        let ccip = self.index.tanks.get(self.tank).is_some_and(|t| t.ccip);
        let alt_txt = match (self.map.as_ref(), self.a) {
            (Some(m), Some(a)) => {
                let g = m.stand(a.x, a.z);
                format!("地面 {:.0}m · 海拔 ALT {:.0}m", g, g + p_cur.gun_h)
            }
            _ => "未放置机位".into(),
        };
        let b_pad_id = match (self.map.as_ref(), self.b) {
            (Some(m), Some(b)) => scene::pad_at(m, b).map(|pd| pd.id.clone()),
            _ => None,
        };
        let has_pads = self.map.as_ref().is_some_and(|m| !m.meta.helipads.is_empty());
        let cap_btns = if heli { scene::capture_zones_labeled(&self.areas()) } else { vec![] };
        let help = [
            ("鼠标左键", "放置己方开火点 / 直升机悬停点 A（直升机支持外圈空域）"),
            ("鼠标右键", "放置目标点 B（陆战与武直均支持设在直升机停机坪及外圈地形）"),
            ("鼠标滚轮", "缩放地图（支持外扩至 20 km 空域；瞄准镜下变倍）"),
            ("2D 拖动", "平移地图"),
            ("3D 拖动", "左键旋转视角；右键或 Shift+拖动平移；双击居中"),
            ("W A S D", "移动战车或飞行直升机（直升机下 Q / E 升降高度）"),
            ("R 键", "开启 / 关闭战场边界显示"),
            ("/ 键", "聚焦地图搜索框"),
        ];
        div().id("sidebar").w(px(364.)).h_full().flex_none().flex().flex_col().gap_3().p_3().overflow_y_scroll().bg(rgb(PANEL)).border_r_1().border_color(rgb(BORDER))
            .child(div().flex().items_center().gap_2().child(div().px_1p5().py_0p5().rounded_md().bg(rgb(COL_A)).text_color(rgb(0x111111)).font_weight(FontWeight::BOLD).child("WT"))
                .child(div().flex().flex_col().child(div().font_weight(FontWeight::BOLD).child("婴儿弹道计算机")).child(div().text_xs().text_color(rgb(MUTED)).child("战争雷霆陆战与武装直升机火箭弹三维弹道解算"))))
            .child(card("地图与任务")
                .child(field("作战地图", Select::new(&self.map_sel).search_placeholder("输入中英文搜索地图…")))
                .child(div().flex().gap_2().child(field("观察视角", Select::new(&self.view_sel))).child(field("游戏模式", Select::new(&self.mode_sel))))
                .child(field("任务布局（不同任务与模式的战区和红区不同）", Select::new(&self.mission_sel).search_placeholder("搜索任务模式…")))
                .child(div().flex().items_center().gap_2()
                    .child(Button::new("high").label("寻找最高点").small().on_click(cx.listener(|this, _, _, cx| this.find_high(cx))))
                    .when(has_pads, |d| d.child(Button::new("fit-air").label("全空域总览").small().on_click(cx.listener(|this, _, _, cx| { this.fit_airspace(); this.v3.dirty(); cx.notify(); }))))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(high_txt))))
            .child(card("载具火炮 / 直升机与弹药")
                .child(div().flex().gap_1p5()
                    .child(Button::new("flt-all").xsmall().when(flt == "all", |b| b.primary()).label(format!("全部 ({n_all})")).flex_1().on_click(cx.listener(|this, _, window, cx| this.set_veh_filter("all", window, cx))))
                    .child(Button::new("flt-gnd").xsmall().when(flt == "ground", |b| b.primary()).label(format!("陆战 ({n_ground})")).flex_1().on_click(cx.listener(|this, _, window, cx| this.set_veh_filter("ground", window, cx))))
                    .child(Button::new("flt-hel").xsmall().when(flt == "heli", |b| b.primary()).label(format!("直升机 ({n_heli})")).flex_1().on_click(cx.listener(|this, _, window, cx| this.set_veh_filter("heli", window, cx)))))
                .child(field("载具 / 直升机型号", Select::new(&self.tank_sel).search_placeholder("搜索坦克、火箭车或武直（如 卡-52 / AH-64 / 直-10）…")))
                .child(field("选用弹药 / 火箭弹", Select::new(&self.shell_sel).search_placeholder("搜索弹种或火箭弹（如 S-8KO / 九头蛇 / S-13）…")))
                .when(!heli && has_pads, |d| {
                    let b_t1n = b_pad_id.as_deref() == Some("t1_near");
                    let b_t1f = b_pad_id.as_deref() == Some("t1_far");
                    let b_t2n = b_pad_id.as_deref() == Some("t2_near");
                    let b_t2f = b_pad_id.as_deref() == Some("t2_far");
                    d.child(div().flex().flex_col().gap_1p5().p_2().rounded_md().bg(rgb(0x131920)).border_1().border_color(rgb(0x2a3b4c))
                        .child(div().flex().justify_between().items_center().text_xs()
                            .child(div().font_weight(FontWeight::BOLD).text_color(rgb(0x52b6ec)).child("反制敌方停机坪（一键设目标 B）"))
                            .child(div().text_color(rgb(MUTED)).child("亦可右键点停机坪")))
                        .child(div().flex().gap_1()
                            .child(Button::new("gnd-b-t1n").xsmall().when(b_t1n, |b| b.primary()).label("1队近H1").flex_1().on_click(cx.listener(|this, _, window, cx| this.target_helipad("t1_near", window, cx))))
                            .child(Button::new("gnd-b-t1f").xsmall().when(b_t1f, |b| b.primary()).label("1队远H2").flex_1().on_click(cx.listener(|this, _, window, cx| this.target_helipad("t1_far", window, cx))))
                            .child(Button::new("gnd-b-t2n").xsmall().when(b_t2n, |b| b.primary()).label("2队近H1").flex_1().on_click(cx.listener(|this, _, window, cx| this.target_helipad("t2_near", window, cx))))
                            .child(Button::new("gnd-b-t2f").xsmall().when(b_t2f, |b| b.primary()).label("2队远H2").flex_1().on_click(cx.listener(|this, _, window, cx| this.target_helipad("t2_far", window, cx))))))
                })
                .when(heli, |d| {
                    d.child(div().flex().flex_col().gap_1p5().p_2().rounded_md().bg(rgb(0x131920)).border_1().border_color(rgb(0x2a3b4c))
                        .child(div().flex().justify_between().items_center().text_xs()
                            .child(div().font_weight(FontWeight::BOLD).text_color(rgb(0x52b6ec)).child("直升机停机坪与悬停参数"))
                            .child(div().text_color(rgb(if ccip { 0x58c472 } else { 0xe0a33a })).child(if ccip { "具备 CCIP" } else { "无 CCIP · 纯标尺" })))
                        .child(div().flex().gap_1()
                            .child(Button::new("hp-t1n").xsmall().label("1队近H1").flex_1().on_click(cx.listener(|this, _, _, cx| this.snap_helipad("t1_near", cx))))
                            .child(Button::new("hp-t1f").xsmall().label("1队远H2").flex_1().on_click(cx.listener(|this, _, _, cx| this.snap_helipad("t1_far", cx))))
                            .child(Button::new("hp-t2n").xsmall().label("2队近H1").flex_1().on_click(cx.listener(|this, _, _, cx| this.snap_helipad("t2_near", cx))))
                            .child(Button::new("hp-t2f").xsmall().label("2队远H2").flex_1().on_click(cx.listener(|this, _, _, cx| this.snap_helipad("t2_far", cx)))))
                        .when(!cap_btns.is_empty(), |d| {
                            d.child(div().flex().gap_1().items_center().child(div().text_xs().text_color(rgb(MUTED)).child("标定B:"))
                                .children(cap_btns.into_iter().enumerate().map(|(ci, (lbl, tx, tz))| {
                                    Button::new(("cap-b", ci)).xsmall().label(lbl).on_click(cx.listener(move |this, _, _, cx| {
                                        this.b = Some(Pt { x: tx, z: tz });
                                        this.update_aim(cx);
                                    }))
                                })))
                        })
                        .child(div().flex().gap_2().items_end()
                            .child(field("悬停离地高 AGL (m)", Input::new(&self.heli_agl_in).small()))
                            .child(field("平飞空速 (km/h)", Input::new(&self.heli_spd_in).small())))
                        .child(div().flex().justify_between().items_center().gap_1()
                            .child(div().flex().gap_1().children([2usize, 35, 75, 150, 300].into_iter().map(|h_val| {
                                Button::new(("agl-q", h_val)).xsmall().label(format!("+{h_val}m")).on_click(cx.listener(move |this, _, window, cx| {
                                    this.heli_agl_in.update(cx, |s, cx| s.set_value(h_val.to_string(), window, cx));
                                    this.update_aim(cx);
                                }))
                            })))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(alt_txt)))
                        .child(div().flex().gap_1().items_center()
                            .child(div().text_xs().text_color(rgb(MUTED)).child("空速预设:"))
                            .children([(0usize, "0悬停"), (120, "120km/h"), (200, "200km/h")].into_iter().map(|(sv, sl)| {
                                Button::new(("spd-q", sv)).xsmall().label(sl).on_click(cx.listener(move |this, _, window, cx| {
                                    this.heli_spd_in.update(cx, |s, cx| s.set_value(sv.to_string(), window, cx));
                                    this.update_aim(cx);
                                }))
                            }))))
                })
                .child(div().flex().gap_2().child(field("最大俯角 °", Input::new(&self.pmin).small())).child(field("最大仰角 °", Input::new(&self.pmax).small())).child(field("扫描射程 m", Input::new(&self.range).small())))
                .when(!heli, |d| d.child(div().flex().gap_2().child(field("火炮离地高 m", Input::new(&self.gun_h).small())).child(field("目标受弹高 m", Input::new(&self.tgt_h).small())))))
            .child(card(if heli { "直升机火箭弹射表（方位角 + 机头仰角）" } else { "弹道解算 A → B" })
                .child(div().flex().flex_col().font_family("Microsoft YaHei UI").text_xs().children(aim_lines.into_iter().map(|(s, k)| {
                    div().min_h(px(15.)).text_color(rgb([0xd8dde2, 0x58c472, 0xe2574c][k.min(2) as usize])).child(s)
                })))
                .child(self.switch("sw-arc", Sw::Arc, "3D 弹道轨迹弧线", Some("显示飞行轨迹、落差垂幕、弹道顶点与落点"), None, cx)))
            .child(card("地图图层与遮挡")
                .child(self.switch("sw-border", Sw::Border, "战场边界", Some("超出作战区域但仍在地图内（越界倒计时区）"), Some(0xe61414), cx))
                .child(self.switch("sw-red", Sw::Red, "禁区红区", Some("出生点保护与禁入杀伤区（进入触发倒计时）"), Some(0x8c0000), cx))
                .child(self.red_list(cx))
                .child(self.switch("sw-caps", Sw::Caps, "占领战区与出生点 / 停机坪", None, Some(0xf2d14b), cx))
                .child(self.switch("sw-water", Sw::Water, "水体", None, Some(0x3c8cc0), cx))
                .child(self.switch("sw-grid", Sw::Grid, "坐标网格（空域 2km / 战场 100m）", None, None, cx))
                .child(self.switch("sw-build", Sw::Build, "3D 建筑与岩石物体", Some("大型建筑、巨石与围墙始终阻挡炮弹"), None, cx))
                .child(self.switch("sw-trees", Sw::Trees, "3D 树木与灌木", None, None, cx))
                .child(self.switch("sw-tanks", Sw::Tanks, "3D 坦克模型", Some("A 点为己方载具，B 点为靶车 T-72A（相对朝向）"), None, cx))
                .child(self.switch("sw-detail", Sw::GroundDetail, "3D 近景地表细节贴图", None, None, cx))
                .child(self.switch("sw-vsight", Sw::VegSight, "树木与灌木阻挡视线", None, None, cx))
                .child(self.switch("sw-vshots", Sw::VegShots, "树木与灌木引爆/阻挡炮弹", None, None, cx)))
            .child(card("全图扫描分析")
                .child(div().flex().gap_2().child(field("3D 地形精度", Select::new(&self.detail_sel))).child(field("扫描计算精度", Select::new(&self.perf_sel))))
                .child(self.switch("sw-clip", Sw::Clip, "仅限作战区域内", Some("反推可打击 B 的阵地时自动排除红区禁区"), None, cx))
                .child(div().flex().gap_2()
                    .child(Button::new("reach").primary().label("A 点火力覆盖").flex_1().on_click(cx.listener(|this, _, _, cx| this.run_sweep(SweepKind::Reach, cx))))
                    .child(Button::new("hit").primary().label("可打到 B 的阵地").flex_1().on_click(cx.listener(|this, _, _, cx| this.run_sweep(SweepKind::Hit, cx)))))
                .child(Button::new("view").primary().label("A 点视野通视分析").on_click(cx.listener(|this, _, _, cx| this.run_sweep(SweepKind::View, cx))))
                .child(div().flex().gap_2()
                    .child(Button::new("clear").label("清除扫描图层").flex_1().on_click(cx.listener(|this, _, _, cx| { this.overlay = None; this.v3.dirty(); cx.notify(); })))
                    .child(Button::new("center").label("居中陆战地图").flex_1().on_click(cx.listener(|this, _, _, cx| { this.center_map(); this.v3.dirty(); cx.notify(); }))))
                .child(div().text_xs().text_color(rgb(MUTED)).child("碰撞说明：中心陆战区含 1m 级 HM2 高程、房屋岩石与树冠遮挡；外圈 15~25km 空域含战雷原版 LandMesh 全域地形高程与卫星地表。")))
            .child(card("操作说明").children(help.map(|(k, v)| div().flex().gap_2().text_xs().child(div().w(px(76.)).font_weight(FontWeight::BOLD).child(k)).child(div().flex_1().text_color(rgb(MUTED)).child(v)))))
    }

    fn legend(&self) -> Option<Vec<(Option<u32>, &'static str)>> {
        let (ov, _) = self.overlay.as_ref()?;
        Some(match ov.kind {
            SweepKind::Reach => vec![(Some(0x58c472), "A 点火力可命中该位置目标")],
            SweepKind::Hit => vec![(None, "可打击目标 B 的阵地："), (Some(0x58c472), "低伸直射"), (Some(0xe98a3c), "仅高抛曲射"), (Some(0x4fb3e8), "直射/曲射皆可")],
            SweepKind::View => vec![(None, "A 点视野："), (Some(0x58c472), "通视可见"), (Some(0x000000), "视野盲区 · 遮挡物："), (Some(0xe23c32), "地形山体"), (Some(0xf09628), "建筑/岩石"), (Some(0xfae63c), "树木/灌木")],
        })
    }
}

fn glass() -> Div {
    div().absolute().px_2().py_1().rounded_md().bg(color(0x0c0e10, 0.82)).border_1().border_color(rgb(BORDER)).text_xs()
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((its, i)) = self.pending_missions.take() {
            self.mission_sel.update(cx, |s, cx| { s.set_items(SearchableVec::new(its), window, cx); s.set_selected_index(Some(IndexPath::new(i)), window, cx); });
        }
        if let Some(i) = self.pending_view.take() {
            self.view_sel.update(cx, |s, cx| s.set_selected_index(Some(IndexPath::new(i)), window, cx));
        }
        let me = cx.entity();
        let border_on = self.sw.on(Sw::Border);
        let view3d = self.view_mode != ViewMode::Map2D;
        if view3d {
            view3d::prepare(self, window, cx);
        }
        let map_canvas = {
            let me = me.clone();
            let me2 = me.clone();
            canvas(
                move |b, _, cx| me2.update(cx, |this, cx| {
                    this.map_bounds = b;
                    if this.center_pending && b.size.width > px(0.) {
                        this.center_pending = false; // the map arrived before the canvas had a size
                        if this.is_heli() { this.fit_airspace(); } else { this.center_map(); }
                        this.v3.dirty();
                        cx.notify();
                    }
                }),
                move |b, _, window, cx| {
                    window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                        me.update(cx, |this, cx| {
                            if this.view_mode == ViewMode::Map2D {
                                crate::draw2d::map(this, b, window, cx);
                            } else {
                                view3d::paint(this, b, window, cx);
                            }
                        })
                    })
                },
            ).size_full()
        };
        let profile = {
            let me = me.clone();
            canvas(|_, _, _| (), move |b, _, window, cx| me.update(cx, |this, cx| crate::draw2d::profile(this, b, window, cx))).size_full()
        };
        let legend = self.legend();
        div().size_full().flex().bg(rgb(BG)).text_color(rgb(0xe6e9ec)).track_focus(&self.focus).on_key_down(cx.listener(Self::key)).on_key_up(cx.listener(Self::key_up))
            .child(self.sidebar(cx))
            .child(div().flex_1().h_full().flex().flex_col().min_w_0()
                .child(div().id("map").relative().flex_1().min_h_0().overflow_hidden()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_scroll(cx.listener(Self::wheel))
                    .child(map_canvas)
                    .when(!self.hud.is_empty(), |d| d.child(glass().top_2().left_2().font_family("Microsoft YaHei UI").child(self.hud.clone())))
                    .when(!self.status.0.is_empty(), |d| d.child(glass().top_2().right_2().when(self.status.1, |d| d.text_color(rgb(0xff8a80))).child(self.status.0.clone())))
                    .child(div().absolute().top_2().w_full().flex().justify_center().child(
                        div().id("btn-red").px_3().py_1().rounded_md().text_xs().font_weight(FontWeight::BOLD).cursor_pointer()
                            .bg(if border_on { color(0x8c1414, 0.9) } else { color(0x0c0e10, 0.82) }).border_1().border_color(rgb(if border_on { 0xe2574c } else { BORDER }))
                            .child(format!("战场边界：{} (R)", if border_on { "开启" } else { "关闭" }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| { let on = !this.sw.on(Sw::Border); this.toggle(Sw::Border, on, cx); }))))
                    .when_some(legend, |d, items| d.child(glass().bottom_2().right_2().flex().items_center().gap_1().children(items.into_iter().map(|(c, t)| {
                        div().flex().items_center().gap_1().when_some(c, |d, c| d.child(div().size_3().rounded_sm().bg(rgb(c)))).child(t)
                    })))))
                .child(div().h(px(220.)).flex_none().border_t_1().border_color(rgb(BORDER)).child(profile)))
    }
}
