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
    pub sw: Switches,

    pub map: Option<Arc<LoadedMap>>,
    map_gen: u64,
    pub map_img: Option<Layer>,
    pub water: Option<Layer>,
    pub zones_img: Option<(String, Layer, f64)>,
    pub overlay: Option<(Overlay, Layer)>,
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
        let view_sel = sel(items(["2D 俯视地图", "3D 自由环绕", "3D 炮口视角（A → B）", "3D 炮长瞄准镜"]), 0, window, cx);
        let shell_sel = sel(vec![], 0, window, cx);
        let detail_sel = sel(items(view3d::DETAIL_LABELS), 2, window, cx);
        let perf_sel = sel(items(Perf::ALL.map(|p| p.label())), 1, window, cx);
        let num = |v: &str, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).default_value(v.to_string()));
        let (pmin, pmax, range, gun_h, tgt_h) = (num("-10", window, cx), num("20", window, cx), num("2500", window, cx), num("2.0", window, cx), num("1.5", window, cx));

        for s in [&map_sel, &tank_sel, &mission_sel, &mode_sel, &view_sel, &shell_sel, &detail_sel, &perf_sel] {
            let which = s.entity_id();
            cx.subscribe_in(s, window, move |this, _, ev: &SelectEvent<SearchableVec<Item>>, window, cx| {
                let SelectEvent::Confirm(Some(v)) = ev else { return };
                this.on_select(which, *v, window, cx);
            }).detach();
        }
        for i in [&pmin, &pmax, &range, &gun_h, &tgt_h] {
            cx.subscribe_in(i, window, |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.update_aim(cx);
                }
            }).detach();
        }
        let sw = Switches([Sw::Border, Sw::Red, Sw::Caps, Sw::Water, Sw::Grid, Sw::Build, Sw::Trees, Sw::Tanks, Sw::GroundDetail, Sw::VegSight, Sw::Arc, Sw::Clip].into_iter().collect());
        let mut this = Self {
            store, index, focus: cx.focus_handle(), map_sel, tank_sel, mission_sel, mode_sel, view_sel, shell_sel, detail_sel, perf_sel,
            pmin, pmax, range, gun_h, tgt_h, sw,
            map: None, map_gen: 0, map_img: None, water: None, zones_img: None, overlay: None,
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

    pub fn shell(&self) -> Option<&Shell> {
        let t = self.index.tanks.get(self.tank)?;
        t.guns.get(self.gun_shell.0)?.shells.get(self.gun_shell.1)
    }

    fn params(&self, cx: &App) -> Params {
        let f = |e: &Entity<InputState>, d: f64| e.read(cx).value().trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(d);
        Params { p_min: f(&self.pmin, -10.0), p_max: f(&self.pmax, 20.0), range: f(&self.range, 2500.0).max(100.0), gun_h: f(&self.gun_h, 2.0), tgt_h: f(&self.tgt_h, 1.5) }
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
            if !entry.objects && !entry.ground {
                return;
            }
            this.update(cx, |this, cx| { this.set_status(format!("正在加载 {} 的建筑掩体、树木与地表贴图…", mtitle), false); cx.notify(); }).ok();
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
                let img = raster::compose(&m);
                (m, img, notes)
            }).await;
            this.update(cx, |this, cx| {
                if this.map_gen != gen_ {
                    return;
                }
                let (m, img, notes) = full;
                let msg = match (&m.obj, notes.first()) {
                    (_, Some(n)) => (n.clone(), true),
                    (Some(o), None) => (format!("已加载 {} 处建筑/岩石掩体，{} 棵树木与灌木", o.inst.len(), o.trees.len()), false),
                    (None, None) => (String::new(), false),
                };
                this.map = Some(Arc::new(m));
                this.map_img = Some(Layer::new(img));
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
        self.map = Some(map);
        self.map_img = Some(Layer::new(img));
        self.zones_img = None;
        self.a = None;
        self.b = None;
        self.heading = None;
        self.aim = None;
        self.overlay = None;
        self.high = None;
        self.hidden_red.clear();
        self.v3.map_changed();
        self.pending_missions = Some((gen_items, mission)); // applied in render, which has the window
        self.set_status("", false);
        self.center_pending = self.map_bounds.size.width <= px(0.);
        self.center_map();
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
            "orbitA" => self.v3.fit(&map.t, cx_ - 600.0, cz - 200.0, 22.0),
            "orbitB" => self.v3.fit(&map.t, cx_ + 600.0, cz + 200.0, 22.0),
            _ => self.v3.fit(&map.t, cx_, cz, 1800.0),
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

    fn pick_tank(&mut self, i: usize, shell_key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.index.tanks.get(i).cloned() else { return };
        self.tank = i;
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

    /// Aim A -> B (exact, on the main thread like the page).
    pub fn update_aim(&mut self, cx: &mut Context<Self>) {
        self.aim = None;
        let p = self.params(cx);
        if let (Some(map), Some(sh), Some(a), Some(b), Some(tc), Some(ts)) = (self.map.as_ref(), self.shell(), self.a, self.b, self.tc(), self.ts()) {
            self.aim = Some(scene::compute_aim(map, tc, ts, sh, a, b, &p));
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
        self.v3.fit(&map.t, self.view.cx, self.view.cz, (x1 - x0).max(z1 - z0));
    }

    fn find_high(&mut self, cx: &mut Context<Self>) {
        let Some(map) = self.map.clone() else { return };
        match scene::highest_point(&map, &self.zones()) {
            Some((x, z, h)) => {
                self.high = Some((self.high_key(), x, z, h));
                self.view.cx = x;
                self.view.cz = z;
                self.v3.fit(&map.t, x, z, 900.0);
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
            self.v3.pick(&map.tr, lx, ly)
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if ev.click_count == 2 && ev.button == MouseButton::Left && self.view_mode != ViewMode::Map2D {
            if let (Some(p), Some(map)) = (self.world_at(ev.position), self.map.clone()) {
                if !self.aim_ready() {
                    self.v3.center_on(&map.t, p);
                    cx.notify();
                }
            }
            return;
        }
        self.drag = Some(Drag { start: ev.position, last: ev.position, btn: ev.button, pan: ev.button != MouseButton::Left || ev.modifiers.shift, moved: false, view: (self.view.cx, self.view.cz) });
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(map), true) = (self.map.clone(), self.map_bounds.contains(&ev.position)) {
            self.hud = match self.world_at(ev.position) {
                Some(p) => format!("X {:.1}  Z {:.1}  海拔 {:.2} m{}", p.x, p.z, map.stand(p.x, p.z), self.a.map_or(String::new(), |a| format!("   距 A {:.1} m", (p.x - a.x).hypot(p.z - a.z)))),
                None => String::new(),
            };
            cx.notify();
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
            if let (Some(map), false) = (self.map.clone(), aim_ready) {
                self.v3.drag(&map.t, sx, sy, pan, f32::from(self.map_bounds.size.height));
            }
        }
        cx.notify();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        if d.moved || !self.map_bounds.contains(&ev.position) {
            return;
        }
        let Some(p) = self.world_at(ev.position) else { return };
        if !self.map.as_ref().is_some_and(|m| m.t.inside(p.x, p.z)) {
            return;
        }
        match d.btn {
            MouseButton::Left => {
                self.a = Some(p);
                self.heading = None; // a newly placed tank faces B
            }
            MouseButton::Right => self.b = Some(p),
            _ => return,
        }
        self.update_aim(cx);
    }

    fn wheel(&mut self, ev: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = -f32::from(ev.delta.pixel_delta(px(40.)).y); // the page's deltaY: positive = away from the user
        if self.view_mode == ViewMode::Map2D {
            let (lx, ly) = self.local(ev.position);
            let (x, z) = (self.wx(lx), self.wz(ly));
            self.view.s = (self.view.s * (-dy as f64 * 0.0015).exp()).clamp(0.01, 40.0);
            self.view.cx = x - (lx - f32::from(self.map_bounds.size.width) / 2.0) as f64 / self.view.s;
            self.view.cz = z + (ly - f32::from(self.map_bounds.size.height) / 2.0) as f64 / self.view.s;
        } else {
            let aim = self.aim_ready();
            self.v3.zoom(dy, aim, self.view_mode == ViewMode::Sight);
        }
        cx.notify();
    }

    /// W/S (or up/down) drive A's tank, A/D (left/right) steer it. Keys only act while the map has the focus,
    /// so typing in a search box or number field never moves the tank or toggles anything.
    fn drive_key(key: &str) -> Option<&'static str> {
        match key {
            "w" | "up" => Some("w"),
            "s" | "down" => Some("s"),
            "a" | "left" => Some("a"),
            "d" | "right" => Some("d"),
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
        const FORWARD: f64 = 12.0; // m/s, about 43 km/h
        const REVERSE: f64 = 5.0;
        const TURN: f64 = 45.0 * crate::core::DEG; // per second
        if !self.focus.is_focused(window) {
            self.drive_keys.clear(); // a key released elsewhere never arrives here
        }
        let (Some(map), Some(a)) = (self.map.clone(), self.a) else { self.drive_keys.clear(); self.driving = false; return false };
        if self.drive_keys.is_empty() {
            self.driving = false;
            return false;
        }
        let on = |k: &str| f64::from(u8::from(self.drive_keys.contains(k)));
        let mut hd = self.heading.unwrap_or_else(|| self.b.map_or(0.0, |b| (b.x - a.x).atan2(b.z - a.z)));
        hd += (on("d") - on("a")) * TURN * dt;
        let speed = on("w") * FORWARD - on("s") * REVERSE;
        let (x, z) = (a.x + hd.sin() * speed * dt, a.z + hd.cos() * speed * dt);
        if map.t.inside(x, z) {
            self.a = Some(Pt { x, z });
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
        let aim_lines: Vec<(String, u8)> = match &self.aim {
            Some(a) => scene::aim_lines(a, &self.params(cx)),
            None => vec![(if self.map.is_some() { "左键点击放置开火点 A，右键点击放置目标点 B。" } else { "请先加载一张地图。" }.to_string(), 0)],
        };
        let help = [
            ("鼠标左键", "放置己方开火点 A"),
            ("鼠标右键", "放置目标点 B"),
            ("鼠标滚轮", "缩放地图（瞄准镜下调节变倍/视场）"),
            ("2D 拖动", "平移地图"),
            ("3D 拖动", "左键旋转视角；右键或 Shift+拖动平移；双击居中"),
            ("W A S D", "驾驶 A 点战车前进/倒车/转向（支持方向键，需先点击地图）"),
            ("R 键", "开启 / 关闭战场边界显示"),
            ("/ 键", "聚焦地图搜索框"),
        ];
        div().id("sidebar").w(px(350.)).h_full().flex_none().flex().flex_col().gap_3().p_3().overflow_y_scroll().bg(rgb(PANEL)).border_r_1().border_color(rgb(BORDER))
            .child(div().flex().items_center().gap_2().child(div().px_1p5().py_0p5().rounded_md().bg(rgb(COL_A)).text_color(rgb(0x111111)).font_weight(FontWeight::BOLD).child("WT"))
                .child(div().flex().flex_col().child(div().font_weight(FontWeight::BOLD).child("婴儿弹道计算机")).child(div().text_xs().text_color(rgb(MUTED)).child("战争雷霆陆战精细地图、战区边界与三维弹道解算"))))
            .child(card("地图与任务")
                .child(field("作战地图", Select::new(&self.map_sel).search_placeholder("输入中英文搜索地图…")))
                .child(div().flex().gap_2().child(field("观察视角", Select::new(&self.view_sel))).child(field("游戏模式", Select::new(&self.mode_sel))))
                .child(field("任务布局（不同任务与模式的战区和红区不同）", Select::new(&self.mission_sel).search_placeholder("搜索任务模式…")))
                .child(div().flex().items_center().gap_2().child(Button::new("high").label("寻找最高点").small().on_click(cx.listener(|this, _, _, cx| this.find_high(cx))))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(high_txt))))
            .child(card("地图图层与遮挡")
                .child(self.switch("sw-border", Sw::Border, "战场边界", Some("超出作战区域但仍在地图内（越界倒计时区）"), Some(0xe61414), cx))
                .child(self.switch("sw-red", Sw::Red, "禁区红区", Some("出生点保护与禁入杀伤区（进入触发倒计时）"), Some(0x8c0000), cx))
                .child(self.red_list(cx))
                .child(self.switch("sw-caps", Sw::Caps, "占领战区与出生点", None, Some(0xf2d14b), cx))
                .child(self.switch("sw-water", Sw::Water, "水体", None, Some(0x3c8cc0), cx))
                .child(self.switch("sw-grid", Sw::Grid, "100 m 坐标网格", None, None, cx))
                .child(self.switch("sw-build", Sw::Build, "3D 建筑与岩石物体", Some("大型建筑、巨石与围墙始终阻挡炮弹"), None, cx))
                .child(self.switch("sw-trees", Sw::Trees, "3D 树木与灌木", None, None, cx))
                .child(self.switch("sw-tanks", Sw::Tanks, "3D 坦克模型", Some("A 点为己方载具，B 点为靶车 T-72A（相对朝向）"), None, cx))
                .child(self.switch("sw-detail", Sw::GroundDetail, "3D 近景地表细节贴图", None, None, cx))
                .child(self.switch("sw-vsight", Sw::VegSight, "树木与灌木阻挡视线", None, None, cx))
                .child(self.switch("sw-vshots", Sw::VegShots, "树木与灌木引爆/阻挡炮弹", None, None, cx)))
            .child(card("载具火炮与弹药")
                .child(field("载具型号", Select::new(&self.tank_sel).search_placeholder("输入中英文或系别搜索载具…")))
                .child(field("选用弹药", Select::new(&self.shell_sel).search_placeholder("搜索弹种或弹药名称…")))
                .child(div().flex().gap_2().child(field("最大俯角 °", Input::new(&self.pmin).small())).child(field("最大仰角 °", Input::new(&self.pmax).small())).child(field("扫描射程 m", Input::new(&self.range).small())))
                .child(div().flex().gap_2().child(field("火炮离地高 m", Input::new(&self.gun_h).small())).child(field("目标受弹高 m", Input::new(&self.tgt_h).small()))))
            .child(card("弹道解算 A → B")
                .child(div().flex().flex_col().font_family("Microsoft YaHei UI").text_xs().children(aim_lines.into_iter().map(|(s, k)| {
                    div().min_h(px(15.)).text_color(rgb([0xd8dde2, 0x58c472, 0xe2574c][k as usize])).child(s)
                })))
                .child(self.switch("sw-arc", Sw::Arc, "3D 弹道轨迹弧线", Some("显示飞行轨迹、落差垂幕、弹道顶点与落点"), None, cx)))
            .child(card("全图扫描分析")
                .child(div().flex().gap_2().child(field("3D 地形精度", Select::new(&self.detail_sel))).child(field("扫描计算精度", Select::new(&self.perf_sel))))
                .child(self.switch("sw-clip", Sw::Clip, "仅限作战区域内", Some("反推可打击 B 的阵地时自动排除红区禁区"), None, cx))
                .child(div().flex().gap_2()
                    .child(Button::new("reach").primary().label("A 点火力覆盖").flex_1().on_click(cx.listener(|this, _, _, cx| this.run_sweep(SweepKind::Reach, cx))))
                    .child(Button::new("hit").primary().label("可打到 B 的阵地").flex_1().on_click(cx.listener(|this, _, _, cx| this.run_sweep(SweepKind::Hit, cx)))))
                .child(Button::new("view").primary().label("A 点视野通视分析").on_click(cx.listener(|this, _, _, cx| this.run_sweep(SweepKind::View, cx))))
                .child(div().flex().gap_2()
                    .child(Button::new("clear").label("清除扫描图层").flex_1().on_click(cx.listener(|this, _, _, cx| { this.overlay = None; this.v3.dirty(); cx.notify(); })))
                    .child(Button::new("center").label("居中重置地图").flex_1().on_click(cx.listener(|this, _, _, cx| { this.center_map(); this.v3.dirty(); cx.notify(); }))))
                .child(div().text_xs().text_color(rgb(MUTED)).child("碰撞说明：采用 HM2 精细高度图 + 大型实体（房屋、巨石、围墙）顶面高度包络计算阻挡，不含门窗镂空或桥底穿洞；勾选树木阻挡时树冠按实心柱体计算；栅栏、路缘与碎石不阻挡炮弹。")))
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
                        this.center_map();
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
