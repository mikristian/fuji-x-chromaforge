//! TouchDesigner TOP project launcher *inside Browse Profiles*.
//!
//! This is a real external TOP round trip through FujiX's non-destructive
//! `photo.editIn` command, not a claim that the Rust develop renderer can run
//! .toe graphs as camera profiles. Its 2-column thumbnails show a real TOP
//! render when available, otherwise the source photo.
//! Click a tile to create a separate stacked TIFF Edit In copy and open that
//! specific TouchDesigner project through the installed bridge. The original
//! RAF, JPEG and .toe files are not touched by this UI.

use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::DevelopSettings;
use serde_json::json;

use crate::icons::{Icon, paint};
#[cfg(not(target_arch = "wasm32"))]
use crate::render::Slot;
use crate::theme::Tokens;
use crate::widgets::{one_line, register};
use crate::{HoverPreview, LccApp};

const COLUMNS: usize = 2;
const THUMB_EDGE: usize = 256;
// The configured local preset directory includes more than 200 genuine
// TouchDesigner projects; never silently cut the library off after 48 cards.
const MAX_PROJECTS: usize = 384;
const SELECTED_ID: &str = "touchdesigner-profile-selected-project";
const FALLBACK_LABEL: &str = "Chromaforge approximation · NOT TouchDesigner render";

fn approximate_plugin(project: &Project) -> Option<&'static str> {
    // Never pass off a visually similar native filter as the user's original
    // .tox. The preview service now executes actual component TOP pixels.
    if project.component {
        return None;
    }
    match project.key.as_str() {
        "VHS#tutorial.toe" => Some("com.fujix.overlay.vhs-tracking"),
        "Glitch Cam.toe" => Some("com.fujix.retro.red-green-vhs"),
        _ if project.name.to_ascii_lowercase().contains("pixel") => Some("com.fujix.retro.chunky-pixel-camera"),
        _ => None,
    }
}

#[cfg(test)]
mod native_approximation_tests {
    use super::*;

    fn project(name: &str, key: &str) -> Project {
        Project { name: name.into(), key: key.into(), path: PathBuf::from(key), output_top: "", component: key.ends_with(".tox") }
    }

    #[test]
    fn mapping_is_bounded_to_known_genres_and_does_not_relabel_other_projects() {
        for (name, file, expected) in [
            ("CRT_Filter", "CRT_Filter.tox", ""),
            ("VHS#tutorial", "VHS#tutorial.toe", "com.fujix.overlay.vhs-tracking"),
            ("Glitch Cam", "Glitch Cam.toe", "com.fujix.retro.red-green-vhs"),
            ("Pixel Crush", "Pixel Crush.toe", "com.fujix.retro.chunky-pixel-camera"),
        ] {
            assert_eq!(approximate_plugin(&project(name, file)), if expected.is_empty() { None } else { Some(expected) });
        }
        assert_eq!(approximate_plugin(&project("Unrelated Film", "Unrelated Film.toe")), None);
        assert_eq!(approximate_plugin(&project("CRT_Filter copy", "CRT_Filter copy.tox")), None);
        assert!(FALLBACK_LABEL.contains("NOT TouchDesigner render"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct PreviewChoice {
    project: Project,
    photo: PhotoId,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
enum PreviewResult {
    Loading,
    Ready(PathBuf),
    Error(String),
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct PreviewJob {
    project: PathBuf,
    photo: PhotoId,
    source_key: u64,
    result: Arc<Mutex<PreviewResult>>,
    texture: Option<egui::TextureHandle>,
}

/// This state belongs to the egui context, shared by Profiles and the main loupe.
/// The worker only writes the output and status; it never touches egui from its thread.
#[cfg(not(target_arch = "wasm32"))]
fn job_id() -> egui::Id {
    egui::Id::new("touchdesigner-real-preview-job")
}

#[cfg(not(target_arch = "wasm32"))]
fn choice_id() -> egui::Id {
    egui::Id::new("touchdesigner-real-preview-choice")
}

#[cfg(not(target_arch = "wasm32"))]
fn pinned_id() -> egui::Id {
    egui::Id::new("touchdesigner-real-preview-pinned")
}

/// Show the external TOP's real output over the main photo when available.
/// Returns true only when a real decoded render was painted.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn paint_real_preview(app: &LccApp, ui: &egui::Ui, painter: &egui::Painter, image_rect: Rect, canvas: Rect) -> bool {
    let ctx = ui.ctx();
    let Some(choice) = ctx.data(|d| d.get_temp::<PreviewChoice>(choice_id())) else { return false };
    let hovering = app.hover_preview.as_ref().is_some_and(|h| h.label.starts_with("TouchDesigner "));
    let pinned = ctx.data(|d| d.get_temp::<bool>(pinned_id())).unwrap_or(false);
    if (!hovering && !pinned) || app.session.active() != Some(choice.photo) {
        return false;
    }
    let source = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == choice.photo);
    let Some(source) = source else { return false };
    // The GPU texture may arrive one frame before its CPU pixel readback.
    // Wait for actual pixels instead of permanently caching an unavailable error.
    let Some(source_pixels) = source.pixels.clone() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        return paint_preview_label(painter, canvas, "TouchDesigner · Waiting for photo pixels…");
    };
    let key = source.key;
    let mut job = ctx.data(|d| d.get_temp::<PreviewJob>(job_id()));
    if job.as_ref().is_none_or(|j| j.project != choice.project.path || j.photo != choice.photo || j.source_key != key) {
        let state = Arc::new(Mutex::new(PreviewResult::Loading));
        job = Some(PreviewJob {
            project: choice.project.path.clone(),
            photo: choice.photo,
            source_key: key,
            result: Arc::clone(&state),
            texture: None,
        });
        let project = choice.project.clone();
        std::thread::spawn(move || {
            let outcome = run_real_preview(&project, &source_pixels, key);
            *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = match outcome {
                Ok(path) => PreviewResult::Ready(path),
                Err(error) => PreviewResult::Error(error),
            };
        });
    }
    let Some(mut job) = job else { return false };
    let result = job.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let (status, is_real) = match result {
        PreviewResult::Loading => {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            ("TouchDesigner · Rendering TOP…".to_string(), false)
        }
        PreviewResult::Error(error) => (format!("TouchDesigner unavailable · {error}"), false),
        PreviewResult::Ready(path) => {
            if job.texture.is_none() {
                match load_real_preview(&path) {
                    Ok(img) => {
                        job.texture =
                            Some(ctx.load_texture(format!("touchdesigner-real-{}-{key}", choice.project.key), img, egui::TextureOptions::LINEAR));
                        // The image lives in the UI texture now. Remove the
                        // generated output to keep hovering large galleries
                        // from exhausting the user's nearly full disk.
                        let _ = std::fs::remove_file(&path);
                    }
                    Err(error) => {
                        *job.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = PreviewResult::Error(error.clone());
                        ctx.data_mut(|d| d.insert_temp(job_id(), job));
                        return paint_preview_label(painter, canvas, &format!("TouchDesigner unavailable · {error}"));
                    }
                }
            }
            (format!("TouchDesigner · {} · TOP rendered", choice.project.name), true)
        }
    };
    if is_real && let Some(texture) = &job.texture {
        let size = texture.size_vec2();
        let scale = (image_rect.width() / size.x).min(image_rect.height() / size.y);
        let r = Rect::from_center_size(image_rect.center(), size * scale);
        painter.with_clip_rect(canvas).image(texture.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    ctx.data_mut(|d| d.insert_temp(job_id(), job));
    paint_preview_label(painter, canvas, &status);
    is_real
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn paint_real_preview(_app: &LccApp, _ui: &egui::Ui, _painter: &egui::Painter, _image_rect: Rect, _canvas: Rect) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn paint_preview_label(painter: &egui::Painter, canvas: Rect, text: &str) -> bool {
    let bounds = Rect::from_min_size(
        canvas.min + vec2(12.0, 12.0),
        vec2((text.chars().count() as f32 * 6.1 + 16.0).min(canvas.width() - 24.0).max(1.0), 24.0),
    );
    painter.rect_filled(bounds, 4.0, Color32::from_black_alpha(205));
    painter.text(bounds.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, text, egui::FontId::proportional(11.0), Color32::WHITE);
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn load_real_preview(path: &Path) -> Result<egui::ColorImage, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 20 * 1024 * 1024 {
        return Err("Unsafe or oversized TOP preview".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    // TouchDesigner Non-Commercial commonly cooks a 1280x960 or 1280x1280
    // canvas even when the source thumbnail is smaller. Those legitimate
    // TOP images exceed 1024*1024 input pixels, so validate their original
    // area up to 2048^2 while still limiting the decoded UI texture to
    // 1024x1024. Real CRT_Filter.tox returned 1280x960 in the live proof.
    let decoded =
        lcc_codecs::decode(&bytes, lcc_codecs::DecodeOptions { max_size: Some((1024, 1024)), max_pixels: 2048 * 2048, ..Default::default() })
            .map_err(|e| format!("TOP image decode failed: {e}"))?;
    let img = decoded.to_srgb8();
    Ok(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()))
}

#[cfg(not(target_arch = "wasm32"))]
fn run_real_preview(project: &Project, pixels: &egui::ColorImage, key: u64) -> Result<PathBuf, String> {
    let home = home().ok_or("Home unavailable")?;
    let folders = [home.join("Downloads"), home.join("Documents/TouchDesigner"), home.join("Documents/TouchDesigner/Profiles")];
    if !folders.iter().any(|p| project.path.parent() == Some(p.as_path())) {
        return Err("TouchDesigner project is outside approved folders".into());
    }
    let valid_extension =
        project.path.extension().is_some_and(|ext| if project.component { ext.eq_ignore_ascii_case("tox") } else { ext.eq_ignore_ascii_case("toe") });
    if !valid_extension {
        return Err("TouchDesigner project extension does not match the selected effect".into());
    }
    // The automatic renderer imports .tox into a disposable network and
    // explicitly validates its in1/out1 TOP ports. Standalone .toe projects
    // require individual source/graph wiring that is not automatic yet.
    if !project.component {
        return Err("Automatic .toe graph preview needs verified TOP input wiring; use the Edit In handoff".into());
    }
    let meta = std::fs::symlink_metadata(&project.path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("Project is unavailable".into());
    }
    let bridge = home.join("Library/Application Support/FujiX Chromaforge/TouchDesigner Bridge");
    let script = bridge.join("render_preview.py");
    if script.is_symlink() || !script.is_file() {
        return Err("Automatic TOP preview renderer is not installed".into());
    }
    let cache = bridge.join("preview-cache");
    if cache.is_symlink() {
        return Err("Unsafe TOP preview cache".into());
    }
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let unique = format!("{}-{key}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos());
    let source = cache.join(format!("{unique}-source.png"));
    let output = cache.join(format!("{unique}-output.png"));
    let result = (|| -> Result<(), String> {
        let edge = 768_usize;
        let [w, h] = pixels.size;
        if w == 0 || h == 0 || w.saturating_mul(h) != pixels.pixels.len() {
            return Err("Empty photo render".into());
        }
        let scale = (edge as f64 / w.max(h) as f64).min(1.0);
        let (tw, th) = (((w as f64 * scale).round() as usize).max(1), ((h as f64 * scale).round() as usize).max(1));
        let mut rgb = lcc_raster::Rgba8::new(tw, th);
        for y in 0..th {
            for x in 0..tw {
                let sx = (x * w / tw).min(w - 1);
                let sy = (y * h / th).min(h - 1);
                if let Some(p) = pixels.pixels.get(sy * w + sx) {
                    rgb.data[y * tw + x] = p.to_array();
                }
            }
        }
        let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&rgb), &lcc_codecs::EncodeMeta::default()).map_err(|e| e.to_string())?;
        std::fs::write(&source, bytes).map_err(|e| e.to_string())?;
        let mut child = std::process::Command::new("/usr/bin/python3")
            .arg(&script)
            .arg("--source")
            .arg(&source)
            .arg("--project")
            .arg(&project.path)
            .arg("--output")
            .arg(&output)
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("TouchDesigner preview could not start: {e}"))?;
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < std::time::Duration::from_secs(40) => std::thread::sleep(std::time::Duration::from_millis(100)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("TOP render timed out or exited unexpectedly".into());
                }
            }
        }
        let finished = child.wait_with_output().map_err(|e| e.to_string())?;
        if !finished.status.success() {
            let reason = String::from_utf8_lossy(&finished.stderr);
            return Err(format!("TOP processing failed: {}", reason.trim().chars().take(160).collect::<String>()));
        }
        load_real_preview(&output).map(|_| ())
    })();
    let _ = std::fs::remove_file(&source);
    if result.is_err() {
        let _ = std::fs::remove_file(&output);
    }
    result.map(|()| output)
}

#[derive(Clone, Debug)]
struct Project {
    name: String,
    path: PathBuf,
    output_top: &'static str,
    key: String,
    component: bool,
}

#[derive(Clone)]
struct ProjectCache {
    until: f64,
    projects: Vec<Project>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Only .toe project / .tox component files one level inside approved folders, including the
/// local preset directory. Never follow symlinks,
/// recurse into user folders, or mistake AppleDouble `._` files for a project.
fn find_projects(home: &Path) -> Vec<Project> {
    let mut out = Vec::new();
    let folders = [home.join("Downloads"), home.join("Documents/TouchDesigner"), home.join("Documents/TouchDesigner/Profiles")];
    for folder in &folders {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for item in entries.flatten() {
            // Prioritize the actual known VHS/Glitch projects *after* reading
            // the folder, so a large Downloads directory cannot hide them.
            let path = item.path();
            let toe = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("toe"));
            let tox = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("tox"));
            if !toe && !tox {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if !meta.is_file() || meta.file_type().is_symlink() {
                continue;
            }
            let Some(file) = path.file_name().and_then(|f| f.to_str()) else { continue };
            if file.starts_with("._") || file.starts_with('.') {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(file);
            let output_top = match file {
                "VHS#tutorial.toe" => "/project1/null1",
                "Glitch Cam.toe" => "/project1/null3",
                _ => "",
            };
            let name = stem.to_string();
            let key = file.to_string();
            out.push(Project { name, path, output_top, key, component: tox });
        }
    }
    // The two real, inspected CRT projects first, then actual CRT/VHS/glitch
    // projects before general TD work; users can search all 200+ projects.
    out.sort_by(|a, b| {
        let order = |p: &Project| match p.key.as_str() {
            "VHS#tutorial.toe" => 0,
            "Glitch Cam.toe" => 1,
            _ => {
                let n = p.name.to_lowercase();
                if ["crt", "vhs", "scan", "glitch", "pixel", "retro", "halftone", "datamosh"].iter().any(|term| n.contains(term)) { 2 } else { 3 }
            }
        };
        order(a).cmp(&order(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    // Cards use the filename as their stable widget ID. Equal filename copies
    // are adjacent after sorting; choose a single file to prevent ID clashes.
    out.dedup_by(|a, b| a.key == b.key);
    out.truncate(MAX_PROJECTS);
    out
}

fn cached_projects(ctx: &egui::Context) -> Vec<Project> {
    let now = ctx.input(|i| i.time);
    let id = egui::Id::new("fujix-touchdesigner-project-files-cache");
    if let Some(cache) = ctx.data(|d| d.get_temp::<ProjectCache>(id))
        && cache.until > now
    {
        return cache.projects;
    }
    let projects = home().map(|home| find_projects(&home)).unwrap_or_default();
    // Mounting and listing a network WebDAV volume is more expensive than
    // reading local Downloads (~2 seconds for the user's 233 projects).
    // Cache for ten minutes; the nearby Refresh button forces a new scan.
    ctx.data_mut(|d| d.insert_temp(id, ProjectCache { until: now + 600.0, projects: projects.clone() }));
    projects
}

/// A one-shot project or reusable-component choice consumed by the external bridge when FujiX's
/// Edit In window hands it the newly created, *derived* TIFF. The selection
/// expires in the bridge and never authorizes writing to the .toe project.
fn queue_project(project: &Project) -> Result<String, String> {
    let home = home().ok_or("Home folder isn't available")?;
    let roots = [home.join("Downloads"), home.join("Documents/TouchDesigner"), home.join("Documents/TouchDesigner/Profiles")];
    let allowed = roots.iter().any(|dir| project.path.parent() == Some(dir.as_path()));
    let suffix_valid =
        project.path.extension().is_some_and(|e| if project.component { e.eq_ignore_ascii_case("tox") } else { e.eq_ignore_ascii_case("toe") });
    if !allowed || !suffix_valid {
        return Err("Only selected TouchDesigner .toe projects and .tox components in approved folders may be used".into());
    }
    let metadata = std::fs::symlink_metadata(&project.path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("TouchDesigner project must be an ordinary existing .toe file".into());
    }
    let bridge = home.join("Library/Application Support/FujiX Chromaforge/TouchDesigner Bridge");
    if bridge.is_symlink() || !bridge.is_dir() {
        return Err("TouchDesigner bridge is not installed; see Photo → Edit In Presets".into());
    }
    let manifest = bridge.join("next_project.json");
    if manifest.is_symlink() {
        return Err("Unsafe bridge selection file".into());
    }
    let time = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?;
    let created_at = time.as_secs();
    let nonce = format!("{}-{}", std::process::id(), time.as_nanos());
    let file = project.path.file_name().and_then(|s| s.to_str()).ok_or("Invalid .toe project filename")?;
    let payload = if project.component {
        json!({
            "kind": "tox",
            "component": file,
            "path": project.path.to_string_lossy(),
            "created_at": created_at,
            "request_nonce": nonce,
        })
    } else {
        json!({
            "kind": "toe",
            "project": file,
            "path": project.path.to_string_lossy(),
            "created_at": created_at,
            "request_nonce": nonce,
            "output_top": project.output_top,
        })
    };
    let tmp = bridge.join(format!(".next_project-{}-{}.tmp", std::process::id(), time.as_nanos()));
    // create_new means another process cannot cause an unexpected rewrite.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        let bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
        file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &manifest).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|_| nonce)
}

fn clear_unconsumed_project(project: &Project, nonce: &str) {
    let Some(home) = home() else { return };
    let pending = home.join("Library/Application Support/FujiX Chromaforge/TouchDesigner Bridge/next_project.json");
    // When Edit In errors before the Mac bridge receives its TIFF, don't
    // let this failed tile selection leak into another later Edit In action.
    if pending.is_symlink() {
        return;
    }
    if let Ok(bytes) = std::fs::read(&pending)
        && let Ok(data) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && data.get("path").and_then(|v| v.as_str()) == Some(project.path.to_string_lossy().as_ref())
        && data.get("request_nonce").and_then(|v| v.as_str()) == Some(nonce)
    {
        let _ = std::fs::remove_file(pending);
    }
}

fn heading(ui: &mut egui::Ui, count: usize) -> bool {
    let id = egui::Id::new("touchdesigner-profile-folder-open");
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    register(ui.ctx(), "touchDesignerProfiles:root", rect);
    crate::widgets::announce(&resp, "TouchDesigner external TOP projects");
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.hover.gamma_multiply(0.6));
    }
    paint(
        ui.painter(),
        Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(16.0, 16.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_label,
    );
    ui.painter().text(pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, "TouchDesigner", t.semibold(13.0), t.text_label);
    ui.painter().text(pos2(rect.right() - 22.0, rect.center().y), Align2::RIGHT_CENTER, count.to_string(), t.font(12.0), t.text_dim);
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    open
}

/// Browse Profiles shows this beside the existing Fujifilm and Basic groups.
/// Source-photo thumbnails use the same 2× 4:3 layout/cached 256px renderer as
/// all other profile cells, with visible warnings that the *effect* is rendered
/// in TouchDesigner, not on the Rust UI thread.
pub(super) fn show_in_profiles(app: &mut LccApp, ui: &mut egui::Ui, photo: PhotoId, base: &DevelopSettings) {
    let projects = cached_projects(ui.ctx());
    // Query installed modules on each view, so a removed or updated plug-in
    // never leaves a stale module hash in settings or in an undoable edit.
    let filters = super::plugin_filters::filter_plugins(&app.session.execute("plugin.list", &json!({})).unwrap_or_default());
    let top = ui.cursor().top();
    if !heading(ui, projects.len()) {
        return;
    }
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 14, top: 6, bottom: 8 }).show(ui, |ui| {
        ui.label(egui::RichText::new("YOUR .TOE PROJECTS + .TOX COMPONENTS · CLICK TO EDIT A COPY").size(10.0).color(t.text_dim));
        ui.label(egui::RichText::new("TOX components use real TouchDesigner TOP preview when the live service is ready. Known TOE looks have clearly marked Chromaforge approximations.").size(10.0).color(t.text_dim));
        ui.label(egui::RichText::new(FALLBACK_LABEL).size(10.0).color(t.text_dim));
        ui.horizontal(|ui| {
            let refresh = ui.small_button("Refresh project files");
            register(ui.ctx(), "touchDesignerProfiles:refresh", refresh.rect);
            if refresh.clicked() {
                ui.ctx().data_mut(|d| d.remove::<ProjectCache>(egui::Id::new("fujix-touchdesigner-project-files-cache")));
            }
        });
    });
    if projects.is_empty() {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 14, top: 4, bottom: 12 }).show(ui, |ui| {
            ui.label("No .toe/.tox files found in Downloads, Documents/TouchDesigner, or Documents/TouchDesigner/Profiles.");
        });
        return;
    }
    let search_id = egui::Id::new("fujix-touchdesigner-gallery-search");
    let mut search = ctx.data(|d| d.get_temp::<String>(search_id)).unwrap_or_default();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 22, right: 16, top: 2, bottom: 8 }).show(ui, |ui| {
        let control =
            ui.add(egui::TextEdit::singleline(&mut search).hint_text("Search CRT, VHS, pixel, glitch, .toe/.tox…").desired_width(f32::INFINITY));
        register(ui.ctx(), "touchDesignerProfiles:search", control.rect);
    });
    ctx.data_mut(|d| d.insert_temp(search_id, search.clone()));
    let query = search.trim().to_lowercase();
    let visible_projects: Vec<&Project> = projects.iter().filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query)).collect();
    if !query.is_empty() {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 22, right: 16, top: 1, bottom: 6 }).show(ui, |ui| {
            ui.label(egui::RichText::new(format!("{} matching projects of {}", visible_projects.len(), projects.len())).size(10.0).color(t.text_dim));
        });
    }
    let width = ui.available_width();
    let gap = 8.0;
    let cw = ((width - 44.0 - gap) / COLUMNS as f32).max(40.0);
    let th = (cw * 0.75).round();
    let mut chosen: Option<Project> = None;
    let mut native_choice: Option<super::plugin_filters::FilterPlugin> = None;
    for row in visible_projects.chunks(COLUMNS) {
        let (rr, _) = ui.allocate_exact_size(vec2(width, th + 39.0), Sense::hover());
        let in_view = ui.is_rect_visible(rr.expand(th));
        for (index, project) in row.iter().enumerate() {
            let cell = Rect::from_min_size(pos2(rr.left() + 22.0 + index as f32 * (cw + gap), rr.top()), vec2(cw, th + 34.0));
            let id = format!("touchDesignerProfiles:tile:{}", project.key);
            let resp = ui.interact(cell, egui::Id::new(&id), Sense::click());
            register(ui.ctx(), id, cell);
            crate::widgets::announce(
                &resp,
                format!("Open {} as a TouchDesigner {}", project.name, if project.component { "TOX component" } else { "external project" }),
            );
            let thumb = Rect::from_min_size(cell.min, vec2(cw, th));
            ui.painter().rect_filled(thumb, 3.0, t.canvas);
            // A real .tox component is a renderable TOP effect. Do not silently
            // substitute a similar-looking Chromaforge WASM filter, especially
            // for the user's authentic CRT_Filter.tox. The real runtime must
            // provide the pixels, or explicitly report unavailable.
            let native_filter =
                if project.component { None } else { approximate_plugin(project).and_then(|id| filters.iter().find(|filter| filter.id == id)) };
            let approximate = native_filter.and_then(|filter| super::plugin_filters::preview_settings(base, filter));
            if in_view
                && let Some(job) = app.session.variant_job(photo, approximate.as_ref().unwrap_or(base), THUMB_EDGE)
                && let Some(tex) = app.renderer.variant(job)
            {
                ui.painter().image(tex.id(), thumb, super::presets::cover_uv(thumb, tex.size), Color32::WHITE);
            }
            #[cfg(not(target_arch = "wasm32"))]
            let real_job = ctx.data(|d| d.get_temp::<PreviewJob>(job_id())).filter(|j| j.project == project.path && j.photo == photo);
            #[cfg(not(target_arch = "wasm32"))]
            if approximate.is_none()
                && let Some(texture) = real_job.as_ref().and_then(|j| j.texture.as_ref())
            {
                ui.painter().image(texture.id(), thumb, super::presets::cover_uv(thumb, texture.size()), Color32::WHITE);
            }
            let strip = Rect::from_min_max(pos2(thumb.left(), thumb.bottom() - 20.0), thumb.right_bottom());
            ui.painter().rect_filled(strip, 2.0, Color32::from_black_alpha(175));
            let selected = ctx.data(|d| d.get_temp::<String>(egui::Id::new(SELECTED_ID))).as_deref() == Some(project.key.as_str());
            ui.painter().text(
                strip.center(),
                Align2::CENTER_CENTER,
                {
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        if approximate.is_some() {
                            "CHROMAFORGE · APPROX"
                        } else if let Some(job) = &real_job {
                            if job.texture.is_some() {
                                "TOP RENDERED"
                            } else if matches!(*job.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner), PreviewResult::Loading) {
                                "RENDERING TOP…"
                            } else {
                                "TOP UNAVAILABLE"
                            }
                        } else if selected {
                            "SELECTED · AWAITING TOP"
                        } else if project.component {
                            "HOVER FOR TOX TOP"
                        } else {
                            "HOVER FOR TOP"
                        }
                    }
                    #[cfg(target_arch = "wasm32")]
                    {
                        if approximate.is_some() { "CHROMAFORGE · APPROX" } else { "TOUCHDESIGNER · EXTERNAL" }
                    }
                },
                t.font(9.0),
                Color32::WHITE,
            );
            if selected {
                ui.painter().rect_stroke(thumb, 3.0, Stroke::new(2.0, Color32::LIGHT_BLUE), egui::StrokeKind::Outside);
            }
            let badge = Rect::from_min_size(thumb.min + vec2(5.0, 5.0), vec2(if project.component { 34.0 } else { 28.0 }, 19.0));
            ui.painter().rect_filled(badge, 3.0, Color32::from_black_alpha(185));
            ui.painter().text(badge.center(), Align2::CENTER_CENTER, if project.component { "TOX" } else { "TD" }, t.semibold(10.0), Color32::WHITE);
            let label = one_line(ui.painter(), &project.name, t.font(12.0), t.text_label, cw - 2.0);
            ui.painter().galley(pos2(cell.left() + 2.0, thumb.bottom() + 13.0 - label.size().y / 2.0), label, t.text_label);
            // A dedicated action keeps the original graph accessible without
            // confusing the one-click approximation with a real TOP render.
            // A small top-right secondary control leaves the thumbnail's
            // center for its primary action: preview on hover and apply the
            // clearly labeled native approximation on click. Previously the
            // external-edit button covered the center, so normal tile clicks
            // launched TouchDesigner instead of visibly changing the photo.
            let open_rect = Rect::from_min_size(pos2(thumb.right() - 36.0, thumb.top() + 4.0), vec2(32.0, 19.0));
            let (open, open_hovered) = if approximate.is_some() || project.component {
                let response = ui.interact(open_rect, egui::Id::new(("td-open-exact", &project.key)), Sense::click());
                register(ui.ctx(), format!("touchDesignerProfiles:open:{}", project.key), open_rect);
                ui.painter().rect_filled(open_rect, 3.0, Color32::from_black_alpha(215));
                ui.painter().text(open_rect.center(), Align2::CENTER_CENTER, "OPEN", t.font(9.0), Color32::WHITE);
                let clicked = response.clicked();
                let hovered = response.hovered();
                response.on_hover_text("Send a separate photo edit copy to the original TouchDesigner project/component");
                (clicked, hovered)
            } else {
                (false, false)
            };
            // The nested "Open in TouchDesigner" button can cover the center
            // of a thumbnail. A pointer over that button must still preview
            // the tile's look, not silently leave the main photo unchanged.
            if resp.hovered() || open_hovered {
                ui.painter().rect_stroke(thumb, 3.0, Stroke::new(1.5, t.text_label), egui::StrokeKind::Outside);
                #[cfg(not(target_arch = "wasm32"))]
                if approximate.is_none() {
                    ctx.data_mut(|d| d.insert_temp(choice_id(), PreviewChoice { project: (**project).clone(), photo }));
                }
                #[cfg(not(target_arch = "wasm32"))]
                if !selected {
                    // Keep the selected TOP on screen after click, even while
                    // the pointer is still over its tile on following frames.
                    ctx.data_mut(|d| d.insert_temp(pinned_id(), false));
                }
                app.hover_preview = Some(HoverPreview {
                    label: if approximate.is_some() {
                        format!("{FALLBACK_LABEL} · {}", project.name)
                    } else {
                        format!(
                            "TouchDesigner {}: {} TOP preview requested",
                            project.name,
                            if project.component { "TOX component" } else { "TOP project" }
                        )
                    },
                    settings: approximate.clone().unwrap_or_else(|| base.clone()),
                });
                ctx.data_mut(|d| d.remove::<(PhotoId, String)>(egui::Id::new("last-profile-hover")));
            }
            if open {
                chosen = Some((**project).clone());
            } else if resp.clicked() {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(SELECTED_ID), project.key.clone()));
                if let Some(filter) = native_filter {
                    native_choice = Some(filter.clone());
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    ctx.data_mut(|d| d.insert_temp(pinned_id(), approximate.is_none()));
                    if approximate.is_none() {
                        ctx.data_mut(|d| d.insert_temp(choice_id(), PreviewChoice { project: (**project).clone(), photo }));
                    }
                }
                if project.component {
                    // The real TOP preview is pinned on the main photograph,
                    // without pretending to persist pixels to the RAW/JPEG.
                    app.ui.status = format!("TouchDesigner · rendering {} TOP preview; original photo unchanged", project.name);
                } else if native_filter.is_none() {
                    chosen = Some((**project).clone());
                }
            }
            resp.on_hover_text(format!(
                "{} · {} · original photo remains untouched",
                project.name,
                if approximate.is_some() { FALLBACK_LABEL } else { "Open original TouchDesigner graph" }
            ));
        }
    }
    if let Some(filter) = native_choice {
        if base.plugin_filters.len() >= lcc_develop::MAX_PLUGIN_FILTERS {
            app.ui.status = "Cannot add approximation: maximum plugin filter count reached".into();
        } else {
            let params = filter.params.as_ref().map(super::plugin_filters::defaults).unwrap_or_default();
            match app.run(
                "develop.pluginFilter.add",
                json!({ "plugin": filter.id, "module_hash": filter.module_hash, "name": filter.name, "params": params }),
            ) {
                Ok(_) => app.ui.status = format!("{FALLBACK_LABEL} · Applied via native plugin filter. Undo is available."),
                Err(error) => app.ui.status = format!("Chromaforge approximation unavailable: {error}"),
            }
        }
    }
    if let Some(project) = chosen {
        match queue_project(&project) {
            Ok(nonce) => {
                let result = app.run(
                    "photo.editIn",
                    json!({
                        "preset": "FujiX TouchDesigner TOP Bridge",
                        "mode": "copyWithAdjustments",
                    }),
                );
                if let Err(error) = result {
                    clear_unconsumed_project(&project, &nonce);
                    app.ui.status = format!("TouchDesigner handoff failed: {error}");
                } else {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(active) = app.session.active() {
                        ctx.data_mut(|d| d.insert_temp(choice_id(), PreviewChoice { project: project.clone(), photo: active }));
                    }
                    app.ui.status = format!(
                        "TouchDesigner: {} {} with a separate TIFF edit copy",
                        if project.component { "importing component" } else { "opening project" },
                        project.name
                    );
                }
            }
            Err(error) => app.ui.status = format!("TouchDesigner project not queued: {error}"),
        }
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 14, top: 6, bottom: 10 }).show(ui, |ui| {
        ui.label(
            egui::RichText::new(
                "TOX previews require actual TouchDesigner TOP pixels. Known TOE approximations are explicitly labeled and are NOT original TouchDesigner renders.",
            )
            .size(10.0)
            .color(t.text_dim),
        );
    });
    let bottom = ui.cursor().top();
    if ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y >= top && p.y < bottom) {
        ctx.data_mut(|d| d.remove::<(PhotoId, String)>(egui::Id::new("last-profile-hover")));
    }
}
