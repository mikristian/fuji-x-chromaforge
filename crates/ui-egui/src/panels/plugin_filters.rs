//! The Edit panel's Plug-in Filters section: the active photo's pixel-filter plug-in stack, one row
//! per entry (enable, name, stage, reorder, remove) with controls generated from the plug-in's
//! manifest parameters. A native two-column gallery previews every available filter on the
//! currently selected photo, using the same async/cached variant renderer as Basic profiles.
//! Hovering a tile temporarily previews the look; clicking adds it as one undoable edit. Every
//! edit goes through `develop.pluginFilter.*` commands; a slider drag is wrapped in
//! `develop.beginInteraction` / `endInteraction`, so it is one History step.

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, pos2, vec2};
use lcc_develop::controls::{ControlSpec, Section, Track};
use lcc_develop::{DevelopSettings, FilterStage, PLUGIN_FILTERS_SECTION, PluginFilter};
use serde_json::{Map, Value, json};

use crate::i18n::tr;
use crate::theme::Tokens;
use crate::widgets::register;
use crate::{HoverPreview, LccApp};

/// An installed filter plug-in as `plugin.list` describes it (missing keys: no controls).
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterPlugin {
    pub id: String,
    pub name: String,
    pub module_hash: String,
    /// The manifest's parameter schema (`{key: {type, label?, …}}`), or `None` when not given.
    pub params: Option<Map<String, Value>>,
    /// Parameter names in the manifest's order (`paramOrder`); keys of `params` it misses follow.
    pub order: Vec<String>,
    pub analyze: bool,
    pub stage: FilterStage,
}

impl FilterPlugin {
    /// The parameter schema in the manifest's order.
    pub(crate) fn ordered_params(&self) -> Vec<(&str, &Value)> {
        let Some(schema) = &self.params else { return Vec::new() };
        let mut out: Vec<(&str, &Value)> = self.order.iter().filter_map(|k| schema.get_key_value(k)).map(|(k, v)| (k.as_str(), v)).collect();
        for (k, v) in schema {
            if !out.iter().any(|(o, _)| *o == k) {
                out.push((k.as_str(), v));
            }
        }
        out
    }
}

/// Installed filter plug-ins from a `plugin.list` reply; entries without an id or module hash are skipped.
pub(crate) fn filter_plugins(list: &Value) -> Vec<FilterPlugin> {
    let Some(all) = list.get("plugins").and_then(Value::as_array) else { return Vec::new() };
    all.iter()
        .filter(|p| p.get("kind").and_then(Value::as_str) == Some("filter"))
        .filter_map(|p| {
            let id = p.get("id").and_then(Value::as_str)?.to_string();
            let module_hash = p.get("moduleHash").and_then(Value::as_str)?.to_string();
            Some(FilterPlugin {
                name: p.get("name").and_then(Value::as_str).unwrap_or(&id).to_string(),
                id,
                module_hash,
                params: p.get("params").and_then(Value::as_object).cloned(),
                order: p
                    .get("paramOrder")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
                analyze: p.get("analyze").and_then(Value::as_bool).unwrap_or(false),
                stage: if p.get("stage").and_then(Value::as_str) == Some("source") { FilterStage::Source } else { FilterStage::Display },
            })
        })
        .collect()
}

/// Defaults from a manifest parameter schema (what a new entry starts with).
pub(super) fn defaults(schema: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, s) in schema {
        let v = match s.get("type").and_then(Value::as_str) {
            Some("number") | Some("int") | Some("bool") => s.get("default").cloned(),
            Some("choice") => s
                .get("default")
                .filter(|d| d.is_string())
                .cloned()
                .or_else(|| s.get("options").and_then(Value::as_array).and_then(|o| o.first()).cloned()),
            _ => None,
        };
        if let Some(v) = v.filter(|v| !v.is_null()) {
            out.insert(k.clone(), v);
        }
    }
    out
}

/// What a row asks for this frame.
enum Action {
    Run(&'static str, Value),
    Begin(String),
    End,
}

pub(crate) fn show(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    super::edit::section(app, ui, d, PLUGIN_FILTERS_SECTION, "Plug-in Filters", body);
}

/// Every frame: stores finished measurements and keeps polling while one runs.
pub(crate) fn pump(app: &mut LccApp, ctx: &egui::Context) {
    if app.session.plugin_analyze_pump() {
        ctx.request_repaint();
    }
    if app.session.plugin_analyses_running() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

const WARN: Color32 = Color32::from_rgb(0xe0, 0xa0, 0x30);
const FILTER_THUMB_EDGE: usize = 256;
const FILTER_GALLERY_COLUMNS: usize = 2;

#[derive(Clone, Default)]
struct FilterGalleryState {
    search: String,
    category: String,
}

fn category_of(filter: &FilterPlugin) -> &'static str {
    let name = filter.name.to_ascii_lowercase();
    if filter.id.starts_with("com.fujix.retro.") {
        "Lo-fi"
    } else if ["crt", "cctv", "camcorder", "vhs", "camera", "surveillance", "security", "overlay", "tv", "tape"]
        .iter()
        .any(|word| name.contains(word))
    {
        "Camera"
    } else if ["mono", "sepia", "black", "white", "noir"].iter().any(|word| name.contains(word)) {
        "Mono"
    } else if ["matte", "retro", "film", "bleach", "fade", "haze", "negative", "grain", "vintage"].iter().any(|word| name.contains(word)) {
        "Film"
    } else {
        "Color"
    }
}

/// Preview settings are built from an exact clone of the *current* photo's
/// develop settings, plus one new filter-stack entry. Never writes the photo
/// while the pointer hovers a gallery tile.
pub(super) fn preview_settings(d: &DevelopSettings, filter: &FilterPlugin) -> Option<DevelopSettings> {
    if d.plugin_filters.len() >= lcc_develop::MAX_PLUGIN_FILTERS {
        return None;
    }
    let mut settings = d.clone();
    let uid = settings.plugin_filters.iter().map(|p| p.uid).max().unwrap_or(0).checked_add(1)?;
    settings.plugin_filters.push(PluginFilter {
        uid,
        plugin: filter.id.clone(),
        module_hash: filter.module_hash.clone(),
        name: filter.name.clone(),
        params: filter.params.as_ref().map(defaults).unwrap_or_default(),
        enabled: true,
        stage: filter.stage,
        analysis: None,
    });
    Some(settings)
}

/// Always-visible visual gallery, not a dropdown or *second scroll area*.
/// The Develop sidebar's single outer scrollbar owns the entire section.
/// Only tiles actually in view ask for a 256px thumbnail. The pipeline
/// deduplicates and caches renders; a library with many plug-ins must never
/// enqueue all images every frame.
fn gallery(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, installed: &[FilterPlugin], actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let state_key = egui::Id::new("fujix-plugin-filter-gallery-state");
    let mut state: FilterGalleryState = ui.ctx().data(|m| m.get_temp(state_key)).unwrap_or_default();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 7, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Filter Gallery").font(t.semibold(13.0)).color(t.text));
            ui.label(RichText::new(format!("{} available", installed.len())).size(10.0).color(t.text_dim));
        });
        ui.label(RichText::new("Hover a look to preview it on your photo. Click to add; Undo always works.").size(10.5).color(t.text_dim));
        let search = ui.add(egui::TextEdit::singleline(&mut state.search).hint_text("Search filters…").desired_width(f32::INFINITY));
        register(ui.ctx(), "pluginFilterGallery:search", search.rect);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for category in ["All", "Film", "Color", "Mono", "Camera", "Lo-fi"] {
                let current = state.category.is_empty() && category == "All" || state.category == category;
                let selected = ui.selectable_label(current, RichText::new(category).size(10.5));
                register(ui.ctx(), format!("pluginFilterGallery:category:{category}"), selected.rect);
                if selected.clicked() {
                    state.category = if category == "All" { "".into() } else { category.into() };
                }
            }
        });
    });

    let query = state.search.trim().to_lowercase();
    let visible: Vec<&FilterPlugin> = installed
        .iter()
        .filter(|p| state.category.is_empty() || category_of(p) == state.category)
        .filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query) || p.id.to_lowercase().contains(&query))
        .collect();
    ui.ctx().data_mut(|m| m.insert_temp(state_key, state));
    if visible.is_empty() {
        inset(ui, |ui| {
            ui.label(
                RichText::new(if installed.is_empty() {
                    "No pixel filters installed. Use File → Plug-ins → Plug-in Manager to install filters."
                } else {
                    "No filters match this search."
                })
                .size(11.0)
                .color(t.text_dim),
            );
        });
        return;
    }
    if app.session.active().is_none() {
        inset(ui, |ui| {
            ui.label(RichText::new("Select a photo to enable visual filter previews.").size(11.0).color(t.text_dim));
        });
    }
    let full = d.plugin_filters.len() >= lcc_develop::MAX_PLUGIN_FILTERS;
    if full {
        inset(ui, |ui| {
            ui.label(RichText::new("Maximum of 32 filters reached. Remove one to add another.").color(WARN));
        });
    }
    // Do NOT add a nested ScrollArea here: it creates a false bottom / scroll
    // trap around 480pt, requiring a second scroll gesture inside the tiles.
    // The right Edit panel already scrolls. The visible-rect check below
    // avoids queueing thumbnails for the gallery rows outside the viewport.
    {
        let gap = 8.0;
        let width = ui.available_width();
        let cell_width = ((width - 32.0 - gap) / FILTER_GALLERY_COLUMNS as f32).max(44.0);
        let image_h = (cell_width * 0.75).round();
        let row_height = image_h + 39.0;
        for row in visible.chunks(FILTER_GALLERY_COLUMNS) {
            let (rr, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
            let in_view = ui.is_rect_visible(rr.expand(image_h));
            for (index, filter) in row.iter().enumerate() {
                let tile = Rect::from_min_size(pos2(rr.left() + 12.0 + index as f32 * (cell_width + gap), rr.top()), vec2(cell_width, row_height));
                let image_rect = Rect::from_min_size(tile.min, vec2(cell_width, image_h));
                let id = format!("pluginFilterGallery:tile:{}", filter.id);
                let resp = ui.interact(tile, egui::Id::new(&id), Sense::click());
                register(ui.ctx(), id, tile);
                crate::widgets::announce(&resp, format!("{}: preview on hover, add on click", filter.name));
                ui.painter().rect_filled(image_rect, 3.0, t.canvas);
                if (in_view || resp.hovered())
                    && let Some(photo) = app.session.active()
                    && let Some(settings) = preview_settings(d, filter)
                {
                    if in_view
                        && let Some(job) = app.session.variant_job(photo, &settings, FILTER_THUMB_EDGE)
                        && let Some(tex) = app.renderer.variant(job)
                    {
                        ui.painter().image(tex.id(), image_rect, super::presets::cover_uv(image_rect, tex.size), Color32::WHITE);
                    }
                    if resp.hovered() {
                        app.hover_preview = Some(HoverPreview { label: format!("Plug-in Filter: {}", filter.name), settings });
                    }
                }
                if resp.hovered() {
                    ui.painter().rect_stroke(image_rect, 3.0, Stroke::new(1.4, t.text_label), egui::StrokeKind::Outside);
                }
                let is_applied = d.plugin_filters.iter().any(|p| p.enabled && p.plugin == filter.id);
                if is_applied {
                    let marker = Rect::from_min_size(image_rect.min + vec2(5.0, 5.0), vec2(43.0, 19.0));
                    ui.painter().rect_filled(marker, 3.0, Color32::from_black_alpha(165));
                    ui.painter().text(marker.center(), Align2::CENTER_CENTER, "ADDED", t.font(9.0), Color32::WHITE);
                }
                let name = filter.name.strip_prefix("FujiX ").unwrap_or(&filter.name);
                let galley = crate::widgets::one_line(ui.painter(), name, t.font(11.5), t.text_label, cell_width - 3.0);
                ui.painter().galley(pos2(tile.left() + 2.0, image_rect.bottom() + 13.0 - galley.size().y / 2.0), galley, t.text_label);
                if resp.clicked() && !full {
                    let params = filter.params.as_ref().map(defaults).unwrap_or_default();
                    actions.push(Action::Run(
                        "develop.pluginFilter.add",
                        json!({"plugin":filter.id,"module_hash":filter.module_hash,"name":filter.name,"params":params}),
                    ));
                }
                resp.on_hover_text(format!("{} · Click to add · Hover previews without saving", filter.name));
            }
        }
    }
}

/// The inset of everything in the section that isn't a slider (sliders inset themselves).
fn inset<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 16, top: 2, bottom: 2 }).show(ui, add).inner
}

/// What a row needs from the session besides the entry.
struct RowState<'a> {
    plugin: Option<&'a FilterPlugin>,
    can_measure: bool,
    measuring: bool,
    measure_error: Option<String>,
    warnings: Vec<&'a str>,
}

fn body(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    // `plugin.list` is only asked while the section is open
    let list = app.session.execute("plugin.list", &json!({})).unwrap_or(Value::Null);
    let installed = filter_plugins(&list);
    let can_measure = lcc_engine::find_command("develop.pluginFilter.analyze").is_some();
    let mut actions: Vec<Action> = Vec::new();
    let photo = app.session.active();
    // the loupe's latest render of this photo: its warnings (a filter skipped or failed)
    let warnings: Vec<String> = photo
        .and_then(|p| app.renderer.textures.get(&crate::render::Slot::Main).filter(|t| t.photo == p))
        .map(|t| t.warnings.clone())
        .unwrap_or_default();
    let n = d.plugin_filters.len();
    if n == 0 {
        inset(ui, |ui| ui.label(RichText::new(tr("No plug-in filters on this photo")).weak()));
    }
    let mut claimed = vec![false; warnings.len()];
    for (i, f) in d.plugin_filters.iter().enumerate() {
        let mut mine = Vec::new();
        for (w, c) in warnings.iter().zip(claimed.iter_mut()) {
            if let Some(m) = lcc_pipeline::plugin_filters::warning_message(w, f) {
                mine.push(m);
                *c = true;
            }
        }
        let st = RowState {
            plugin: installed.iter().find(|p| p.id == f.plugin),
            can_measure,
            measuring: photo.is_some_and(|p| app.session.plugin_analyzing(p, f.uid)),
            measure_error: photo.and_then(|p| app.session.plugin_analyze_error(p, f.uid)).map(str::to_string),
            warnings: mine,
        };
        ui.push_id(("pluginFilter", f.uid), |ui| row(ui, f, i, n, &st, &mut actions));
        inset(ui, |ui| ui.separator());
    }
    inset(ui, |ui| {
        // Render-level warnings (not about one particular filter) stay visible.
        for (w, _) in warnings.iter().zip(&claimed).filter(|(_, c)| !**c) {
            ui.label(RichText::new(format!("⚠ {w}")).color(WARN));
        }
    });
    gallery(app, ui, d, &installed, &mut actions);
    for a in actions {
        // errors surface through the app's command error path; nothing here can corrupt the stack
        let _ = match a {
            Action::Run(id, p) => app.run(id, p),
            Action::Begin(label) => app.run("develop.beginInteraction", json!({"label": label})),
            Action::End => app.run("develop.endInteraction", json!({})),
        };
    }
}

fn row(ui: &mut egui::Ui, f: &PluginFilter, i: usize, n: usize, st: &RowState, out: &mut Vec<Action>) {
    let uid = f.uid;
    let plugin = st.plugin;
    inset(ui, |ui| {
        ui.horizontal(|ui| {
            let mut on = f.enabled;
            let r = ui.checkbox(&mut on, "");
            crate::access::label(&r, "Enabled");
            if r.changed() {
                out.push(Action::Run("develop.pluginFilter.set", json!({"uid": uid, "enabled": on})));
            }
            ui.label(RichText::new(&f.name).strong());
            if plugin.is_none() {
                ui.label(RichText::new(format!("⚠ {}", tr("not installed"))).color(WARN))
                    .on_hover_text(tr("This filter's plug-in is not installed: it is skipped and its settings are kept."));
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Stage")).strong()).on_hover_text(tr(
                "Source: on the decoded raw, before white balance and tone (e.g. negative conversion). Display: after the develop adjustments.",
            ));
            let name = |s: FilterStage| match s {
                FilterStage::Source => tr("Source"),
                FilterStage::Display => tr("Display"),
            };
            egui::ComboBox::from_id_salt("stage").width(76.0).selected_text(name(f.stage)).show_ui(ui, |ui| {
                for (s, key) in [(FilterStage::Source, "source"), (FilterStage::Display, "display")] {
                    if ui.selectable_label(f.stage == s, name(s)).clicked() && f.stage != s {
                        out.push(Action::Run("develop.pluginFilter.set", json!({"uid": uid, "stage": key})));
                    }
                }
            });
            if st.can_measure && plugin.is_some_and(|p| p.analyze) {
                if st.measuring {
                    ui.add(egui::Spinner::new().size(12.0));
                    let r = ui.add_enabled(false, egui::Button::new(tr("Measuring…")));
                    register(ui.ctx(), format!("pluginFilterMeasure:{uid}"), r.rect);
                } else {
                    let r = ui.button(tr("Measure"));
                    register(ui.ctx(), format!("pluginFilterMeasure:{uid}"), r.rect);
                    if r.clicked() {
                        out.push(Action::Run("develop.pluginFilter.analyze", json!({"uid": uid})));
                    }
                }
            }
            if ui.add_enabled(i > 0, egui::Button::new("▲").small()).on_hover_text(tr("Move Up")).clicked() {
                out.push(Action::Run("develop.pluginFilter.move", json!({"uid": uid, "index": i.saturating_sub(1)})));
            }
            if ui.add_enabled(i + 1 < n, egui::Button::new("▼").small()).on_hover_text(tr("Move Down")).clicked() {
                out.push(Action::Run("develop.pluginFilter.move", json!({"uid": uid, "index": i + 1})));
            }
            if ui.small_button("×").on_hover_text(tr("Remove")).clicked() {
                out.push(Action::Run("develop.pluginFilter.remove", json!({"uid": uid})));
            }
        });
        if let Some(e) = &st.measure_error {
            ui.label(RichText::new(format!("⚠ {}: {e}", tr("Measure"))).color(WARN));
        }
        for w in &st.warnings {
            ui.label(RichText::new(format!("⚠ {w}")).color(WARN));
        }
    });
    let Some(p) = plugin.filter(|p| p.params.is_some()) else {
        // no plug-in or no schema: values shown read-only
        inset(ui, |ui| {
            for (k, v) in &f.params {
                ui.label(RichText::new(format!("{}: {v}", lcc_filters_label(k, None))).weak());
            }
        });
        return;
    };
    for (k, s) in p.ordered_params() {
        param(ui, uid, &f.name, k, s, f.params.get(k), out);
    }
}

/// A parameter's label: the manifest's `label` (as `plugin.list` gives it), else the key humanised.
fn lcc_filters_label(key: &str, spec: Option<&Value>) -> String {
    match spec.and_then(|s| s.get("label")).and_then(Value::as_str).filter(|l| !l.trim().is_empty()) {
        Some(l) => l.to_string(),
        None => humanise(key),
    }
}

/// `dynamic_range` / `dynamicRange` → "Dynamic range".
pub(crate) fn humanise(key: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if matches!(c, '_' | '-' | ' ') {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        let acronym = w.chars().count() > 1 && w.chars().all(|c| !c.is_lowercase());
        let w = if acronym { w.clone() } else { w.to_lowercase() };
        if i == 0 {
            let mut cs = w.chars();
            if let Some(f) = cs.next() {
                out.extend(f.to_uppercase());
                out.push_str(cs.as_str());
            }
        } else {
            out.push(' ');
            out.push_str(&w);
        }
    }
    if out.is_empty() { key.to_string() } else { out }
}

/// A `'static` control id for a plug-in parameter slider (the develop slider widget keys its state and
/// its control-channel name on it). Interned once per name, at most [`MAX_IDS`] of them.
fn slider_id(uid: u32, key: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    const MAX_IDS: usize = 4096;
    static IDS: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let name = format!("pluginFilter.{uid}.{key}");
    let mut ids = IDS.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(id) = ids.get(&name) {
        return id;
    }
    if ids.len() >= MAX_IDS {
        return "pluginFilter.param";
    }
    let id: &'static str = Box::leak(name.clone().into_boxed_str());
    ids.insert(name, id);
    id
}

/// The develop control spec a numeric parameter's slider uses.
fn slider_spec(uid: u32, key: &str, lo: f64, hi: f64, def: f64, int: bool) -> ControlSpec {
    let span = hi - lo;
    let decimals: u8 = if int {
        0
    } else if span <= 10.0 {
        2
    } else if span <= 100.0 {
        1
    } else {
        0
    };
    ControlSpec {
        id: slider_id(uid, key),
        label: "",
        section: Section::Effects,
        min: lo,
        max: hi,
        default: def.clamp(lo, hi),
        step: 10f64.powi(-i32::from(decimals)),
        decimals,
        track: if lo < 0.0 && hi > 0.0 { Track::Centered } else { Track::Plain },
        shade: lcc_develop::Shade::Plain,
    }
}

fn param(ui: &mut egui::Ui, uid: u32, name: &str, key: &str, s: &Value, cur: Option<&Value>, out: &mut Vec<Action>) {
    let label = lcc_filters_label(key, Some(s));
    let set = |v: Value| Action::Run("develop.pluginFilter.set", json!({"uid": uid, "params": {key: v}}));
    match s.get("type").and_then(Value::as_str) {
        Some(t @ ("number" | "int")) => {
            let int = t == "int";
            let (lo, hi) = match (s.get("min").and_then(Value::as_f64), s.get("max").and_then(Value::as_f64)) {
                (Some(lo), Some(hi)) => (lo, hi),
                // ints default to a wide range, as the manifest does
                _ if int => (-1_000_000.0, 1_000_000.0),
                _ => return,
            };
            if !(lo.is_finite() && hi.is_finite()) || lo == hi {
                return;
            }
            let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
            let def = s.get("default").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(lo);
            let spec = slider_spec(uid, key, lo, hi, def, int);
            let v = cur.and_then(Value::as_f64).unwrap_or(def).clamp(lo, hi);
            let o = crate::widgets::slider(ui, &spec, v, ui.is_enabled(), Some(&label));
            if o.drag_started && !o.reset {
                out.push(Action::Begin(format!("Plug-in: {name} › {label}")));
            }
            if let Some(nv) = o.value.filter(|nv| nv.is_finite()) {
                out.push(set(if int { json!(nv.round() as i64) } else { json!(nv) }));
            }
            if o.drag_stopped && !o.reset {
                out.push(Action::End);
            }
        }
        Some("bool") => inset(ui, |ui| {
            let mut b = cur.and_then(Value::as_bool).or_else(|| s.get("default").and_then(Value::as_bool)).unwrap_or(false);
            let r = ui.checkbox(&mut b, &label);
            if r.changed() {
                out.push(set(json!(b)));
            }
        }),
        Some("choice") => {
            let Some(opts) = s.get("options").and_then(Value::as_array) else { return };
            let now = cur.and_then(Value::as_str).or_else(|| s.get("default").and_then(Value::as_str)).unwrap_or("");
            inset(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&label);
                    let r = egui::ComboBox::from_id_salt(("choice", key)).selected_text(now).show_ui(ui, |ui| {
                        let mut pick = |ui: &mut egui::Ui, o: &str| {
                            if ui.selectable_label(o == now, o).clicked() && o != now {
                                out.push(set(json!(o)));
                            }
                        };
                        let opts: Vec<&str> = opts.iter().filter_map(Value::as_str).collect();
                        // the manifest's `groups`: one collapsible section per group, in order of first
                        // appearance; the one holding the current choice starts open
                        match choice_groups(s, &opts) {
                            None => opts.iter().for_each(|o| pick(ui, o)),
                            Some(groups) => {
                                let mut shown: Vec<&str> = Vec::new();
                                for (o, g) in opts.iter().zip(&groups) {
                                    if g.is_empty() {
                                        pick(ui, o);
                                    } else if !shown.contains(g) {
                                        shown.push(g);
                                        let open = opts.iter().zip(&groups).any(|(o, h)| h == g && *o == now);
                                        egui::CollapsingHeader::new(*g).id_salt(("choice-group", key, *g)).default_open(open).show(ui, |ui| {
                                            opts.iter().zip(&groups).filter(|(_, h)| *h == g).for_each(|(o, _)| pick(ui, o));
                                        });
                                    }
                                }
                            }
                        }
                    });
                    register(ui.ctx(), format!("pluginFilterChoice:{uid}:{key}"), r.response.rect);
                });
            });
        }
        _ => {}
    }
}

/// The menu group of each of `opts` from a choice spec's `groups` (as `plugin.list` gives them),
/// when there is one per option and at least one isn't empty.
fn choice_groups<'a>(spec: &'a Value, opts: &[&str]) -> Option<Vec<&'a str>> {
    let g: Vec<&str> = spec.get("groups")?.as_array()?.iter().map(Value::as_str).collect::<Option<_>>()?;
    (g.len() == opts.len() && g.iter().any(|x| !x.is_empty())).then_some(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_groups_need_one_per_option() {
        let opts = ["Generic", "Portra", "Superia"];
        assert_eq!(choice_groups(&json!({"groups": ["", "Kodak", "Fujifilm"]}), &opts), Some(vec!["", "Kodak", "Fujifilm"]));
        for bad in [
            json!({}),
            json!({"groups": ["", "Kodak"]}),
            json!({"groups": ["", "", ""]}),
            json!({"groups": ["", 3, "x"]}),
            json!({"groups": "Kodak"}),
        ] {
            assert_eq!(choice_groups(&bad, &opts), None, "{bad}");
        }
    }

    #[test]
    fn plugin_list_parsing_is_defensive() {
        let list = json!({"plugins": [
            {"id": "a.neg", "name": "Negative", "kind": "filter", "moduleHash": "h1", "analyze": true,
             "params": {"gamma": {"type": "number", "min": 1.0, "max": 3.0, "default": 2.2},
                        "mode": {"type": "choice", "options": ["c41", "bw"]},
                        "flip": {"type": "bool"}}},
            {"id": "b.cmd", "name": "Command", "kind": "command"},
            {"id": "c.nohash", "kind": "filter"},
            {"kind": "filter", "moduleHash": "x"},
            "junk",
        ]});
        let f = filter_plugins(&list);
        assert_eq!(f.len(), 1);
        assert!(f[0].analyze);
        let d = defaults(f[0].params.as_ref().unwrap());
        assert_eq!(d.get("gamma"), Some(&json!(2.2)));
        assert_eq!(d.get("mode"), Some(&json!("c41")));
        assert!(d.get("flip").is_none(), "no default given: the plug-in's own applies");
        assert!(filter_plugins(&Value::Null).is_empty());
        // no paramOrder: every key still shows
        assert_eq!(f[0].ordered_params().len(), 3);
        assert!(filter_plugins(&json!({"plugins": 3})).is_empty());
    }

    #[test]
    fn params_follow_the_manifest_order_and_get_labels() {
        let list = json!({"plugins": [{"id": "a", "kind": "filter", "moduleHash": "h",
            "params": {"a": {"type": "bool"}, "dynamic_range": {"type": "bool"}, "temp": {"type": "bool", "label": "Temperature"}},
            "paramOrder": ["temp", "dynamic_range", "gone", "a"]}]});
        let f = filter_plugins(&list);
        let keys: Vec<&str> = f[0].ordered_params().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["temp", "dynamic_range", "a"]);
        let labels: Vec<String> = f[0].ordered_params().iter().map(|(k, s)| lcc_filters_label(k, Some(s))).collect();
        assert_eq!(labels, ["Temperature", "Dynamic range", "A"]);
        assert_eq!(humanise("dynamicRange"), "Dynamic range");
    }
}
