//! Develop's tool strip as Lightroom Classic has it (fork-owned): a row of the editing tools under
//! the histogram (Crop, Remove, Red Eye, Masking, in the user's order), each opening its drawer
//! in the Develop panels' place: while a tool is in use the panel holds only its own controls, so
//! the photo's global sliders don't crowd it (#618). A click on the tool in use closes its drawer,
//! and the panels come back, as Return and the drawer's Close do. A child module of `right`, so it
//! uses that file's helpers. The row starts with an Edit button that shows the Develop panels
//! (closing any drawer), and a dot under a tool tells that it has been applied to the photo (a crop,
//! spots, masks, red-eye fixes; #617).

use egui::vec2;
use serde_json::json;

use crate::LccApp;
use crate::icons::Icon;
use crate::module::{ModuleId, PanelId};
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button_named};

/// Height of the row.
const ROW_H: f32 = 34.0;
/// Width of one tool's button.
const BUTTON_W: f32 = 40.0;

/// Whether `right` shows Develop's histogram, tool row and panels (the Edit panel, a tool's drawer
/// over it, or the profile browser in its place).
pub(super) fn develop_tools(app: &LccApp) -> bool {
    app.ui.module == ModuleId::Develop && app.ui.right.is_edit_tool()
}

/// The row of tools under the histogram, centred, with a rule under it.
pub(super) fn row(app: &mut LccApp, ui: &mut egui::Ui) {
    let has_photo = app.session.active().is_some();
    let tools: Vec<PanelId> = crate::panels::strip::strip_panels(app).into_iter().filter(|p| *p != PanelId::Edit).collect();
    let develop = app.session.active().and_then(|id| app.session.develop_of(id));
    let width = ui.available_width();
    // Chromaforge's Classic right column can be narrow. The camera manager is
    // also reachable from the Fuji folder in Presets and the Window menu, so
    // collapse that shortcut before crowding/losing the ordinary edit tools.
    let show_camera_shortcut = width >= (tools.len() + 3) as f32 * 34.0 + (tools.len() + 2) as f32 * 4.0;
    let count = tools.len() + if show_camera_shortcut { 3 } else { 2 };
    let button_w = ((width - (count.saturating_sub(1)) as f32 * 4.0) / count as f32)
        .min(BUTTON_W).max(26.0);
    let occupied = button_w * count as f32 + 4.0 * (count.saturating_sub(1)) as f32;
    ui.allocate_ui_with_layout(vec2(width, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // Keep the native photo presets and the Fuji camera manager alongside
        // the editing tools. Fuji camera recipes appear in the real Presets
        // tree, rather than occupying a second, duplicate presets drawer.
        ui.add_space(((width - occupied) / 2.0).max(0.0));
        // Edit: the Develop panels, with no tool's drawer over them
        let edit_on = app.ui.right == RightPanel::Edit;
        let sc = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "panel.edit");
        let tip = crate::widgets::command_tooltip(ui, "Edit", sc);
        let resp = icon_button_named(ui, "toolsEdit", Icon::Sliders, vec2(button_w, ROW_H - 4.0), edit_on, has_photo, crate::i18n::tr("Edit"), &tip);
        if resp.clicked() && !edit_on {
            let _ = app.run("panel.edit", json!({}));
        }
        let presets_on = app.ui.left_panel && !app.ui.presets
            && !app.ui.hidden_panels.contains(&PanelId::Presets)
            && crate::panels::classic::is_open_with_key(app, crate::panels::presets::IN_COLUMN_KEY);
        let presets = icon_button_named(ui, "toolsPresets", Icon::Presets,
            vec2(button_w, ROW_H - 4.0), presets_on, true, "Presets",
            "Open the native Presets panel (including Fuji X100VI Recipes)");
        if presets.clicked() {
            crate::panels::fujix::toggle_native_presets(app);
        }
        if show_camera_shortcut {
            let camera_on = app.ui.right == RightPanel::FujiCamera;
            let camera = icon_button_named(ui, "fujixCamera", Icon::Photos,
                vec2(button_w, ROW_H - 4.0), camera_on, true, "X100VI Camera",
                "X100VI C1–C7 camera settings and read-only USB tools");
            if camera.clicked() {
                let _ = app.run(if camera_on { "panel.edit" } else { "panel.fujiCamera" }, json!({}));
            }
        }
        for p in &tools {
            let Some((id, icon, panel, tip, needs_photo)) = crate::panels::strip::entry(*p) else { continue };
            let on = app.ui.right == panel;
            let video = crate::video_ui::blocked(app, &format!("panel.{id}"));
            let sc = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, &format!("panel.{id}"));
            let tip = if video { crate::i18n::tr(crate::video_ui::NOT_FOR_VIDEO).to_string() } else { crate::widgets::command_tooltip(ui, tip, sc) };
            let name = crate::i18n::tr(entry_name(*p));
            let resp = icon_button_named(ui, id, icon, vec2(button_w, ROW_H - 4.0), on, (has_photo || !needs_photo) && !video, name, &tip);
            if develop.as_ref().is_some_and(|d| applied(d, panel)) {
                // the tool has been applied to this photo: a dot under its icon
                let t = Tokens::get(ui.ctx());
                ui.painter().circle_filled(egui::pos2(resp.rect.center().x, resp.rect.bottom() - 1.0), 2.0, t.pick);
            }
            if resp.clicked() {
                // the tool in use: its drawer closes, back to the panels
                let cmd = if on { "panel.edit".to_string() } else { format!("panel.{id}") };
                let _ = app.run(&cmd, json!({}));
            }
            // the strip's header menu: show/hide, solo, hiding mode
            crate::panels::classic::side_menu(app, &resp, crate::panels::strip::strip_declared(ModuleId::Develop));
        }
    });
    divider(ui);
}

/// Whether a tool has left something on the photo's settings.
fn applied(d: &lcc_develop::DevelopSettings, tool: RightPanel) -> bool {
    match tool {
        RightPanel::Crop => d.crop != lcc_develop::Crop::default(),
        RightPanel::Remove => !d.spots.is_empty(),
        RightPanel::Masking => !d.masks.is_empty(),
        RightPanel::RedEye => !d.red_eye.is_empty(),
        _ => false,
    }
}

/// A tool's name for screen readers (its tooltip carries the shortcut).
fn entry_name(p: PanelId) -> &'static str {
    crate::panels::strip::entry(p).map_or("", |e| e.3)
}
