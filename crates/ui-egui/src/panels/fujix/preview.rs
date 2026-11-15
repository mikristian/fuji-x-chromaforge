//! Non-destructive, *approximate* Fujifilm recipe previews using Chromaforge's
//! existing Rust develop renderer. The X-Processor's film simulations, Color
//! Chrome and grain are proprietary, so these are explicitly not camera-accurate
//! conversions. No catalog edits or camera commands are issued here.

use lcc_develop::{DevelopSettings, Treatment};
use super::settings::CameraSettings;

/// Preview a recipe on top of the photo's current (unmodified) Develop state.
/// Empty camera slots have no look and must not manufacture one.
pub(super) fn approximate(base: &DevelopSettings, camera: &CameraSettings) -> Option<DevelopSettings> {
    if camera.values.is_empty() { return None; }
    let mut out = base.clone();
    let simulation = camera.get("film_simulation").trim().to_ascii_lowercase();
    let mapped_simulation = film_profile(&simulation).is_some();
    if !simulation.is_empty() {
        let profile = film_profile(&simulation);
        if let Some(profile) = profile {
            out.profile.id = profile.to_string();
            out.profile.amount = 100.0;
            out.treatment = if profile == "lc.mono" || profile.starts_with("lc.bw.") {
                Treatment::Bw
            } else {
                Treatment::Color
            };
        }
        // Film simulations sharing a closest desktop profile still get their
        // own understated tonal character. These are aesthetic estimates, not
        // claims that the camera's proprietary color science is implemented.
        match simulation.as_str() {
            s if s.contains("velvia") => { out.color.saturation = add(out.color.saturation, 18.0, -100.0, 100.0); out.light.contrast = add(out.light.contrast, 9.0, -100.0, 100.0); }
            s if s.contains("classic negative") => { out.light.contrast = add(out.light.contrast, 13.0, -100.0, 100.0); out.color.saturation = add(out.color.saturation, -7.0, -100.0, 100.0); }
            s if s.contains("classic chrome") => { out.color.saturation = add(out.color.saturation, -12.0, -100.0, 100.0); }
            s if s.contains("bleach bypass") => { out.color.saturation = add(out.color.saturation, -18.0, -100.0, 100.0); out.light.contrast = add(out.light.contrast, 16.0, -100.0, 100.0); }
            s if s.contains("eterna") => { out.light.contrast = add(out.light.contrast, -11.0, -100.0, 100.0); out.color.saturation = add(out.color.saturation, -9.0, -100.0, 100.0); }
            s if s.contains("pro neg. hi") => { out.light.contrast = add(out.light.contrast, 10.0, -100.0, 100.0); }
            s if s.contains("pro neg. std") => { out.light.contrast = add(out.light.contrast, -5.0, -100.0, 100.0); }
            s if s.contains("nostalgic") => { out.wb.temp = add(out.wb.temp, 220.0, 2000.0, 50000.0); }
            _ => {}
        }
    }

    if let Some(n) = camera.number("highlight") {
        out.light.highlights = add(out.light.highlights, n * 18.0, -100.0, 100.0);
    }
    if let Some(n) = camera.number("shadow") {
        out.light.shadows = add(out.light.shadows, -n * 17.0, -100.0, 100.0);
    }
    if let Some(n) = camera.number("color") {
        out.color.saturation = add(out.color.saturation, n * 9.0, -100.0, 100.0);
    }
    if let Some(n) = camera.number("sharpness") {
        out.detail.sharpen_amount = add(out.detail.sharpen_amount, n * 7.0, 0.0, 150.0);
    }
    if let Some(n) = camera.number("noise_reduction") {
        out.detail.nr_luminance = add(out.detail.nr_luminance, n * 4.0, 0.0, 100.0);
    }
    if let Some(n) = camera.number("clarity") {
        out.effects.clarity = add(out.effects.clarity, n * 10.0, -100.0, 100.0);
    }
    match camera.get("grain_strength").to_ascii_lowercase().as_str() {
        "weak" => out.grain.amount = 26.0,
        "strong" => out.grain.amount = 55.0,
        "off" => out.grain.amount = 0.0,
        _ => {}
    }
    match camera.get("grain_size").to_ascii_lowercase().as_str() {
        "small" => out.grain.size = 23.0,
        "large" => out.grain.size = 57.0,
        _ => {}
    }
    let chrome = camera.get("color_chrome").to_ascii_lowercase();
    if chrome == "weak" || chrome == "strong" {
        out.color.vibrance = add(out.color.vibrance, if chrome == "strong" { 9.0 } else { 4.0 }, -100.0, 100.0);
    }
    let blue = camera.get("color_chrome_blue").to_ascii_lowercase();
    if blue == "weak" || blue == "strong" {
        out.mixer.blue.sat = add(out.mixer.blue.sat, if blue == "strong" { 16.0 } else { 7.0 }, -100.0, 100.0);
        out.mixer.blue.lum = add(out.mixer.blue.lum, if blue == "strong" { -10.0 } else { -4.0 }, -100.0, 100.0);
    }
    if let Some(n) = camera.number("wb_red") {
        out.wb.tint = add(out.wb.tint, n * 3.0, -150.0, 150.0);
    }
    if let Some(n) = camera.number("wb_blue") {
        out.wb.temp = add(out.wb.temp, -n * 65.0, 2000.0, 50000.0);
    }
    if let Some(temp) = camera.get("white_balance_kelvin").trim_end_matches(['K', 'k']).trim().parse::<f64>().ok()
        && (2000.0..=50000.0).contains(&temp) {
        out.wb.temp = temp;
    }
    match camera.get("dynamic_range").to_ascii_lowercase().as_str() {
        "dr200" => out.light.highlights = add(out.light.highlights, -10.0, -100.0, 100.0),
        "dr400" => out.light.highlights = add(out.light.highlights, -22.0, -100.0, 100.0),
        _ => {}
    }
    // ISO and exposure compensation are reference notes, not a safely
    // inferred physical exposure transform.
    // Reference-only ISO/EV notes have no corresponding desktop treatment.
    // Never present an unchanged image as a recipe simulation unless an
    // actual known film simulation (e.g. neutral Provia) was selected.
    if !mapped_simulation && out == *base { None } else { Some(out) }
}

fn add(value: f64, delta: f64, min: f64, max: f64) -> f64 {
    (value + delta).clamp(min, max)
}

fn film_profile(value: &str) -> Option<&'static str> {
    let s = value.trim().to_ascii_lowercase();
    Some(match s.as_str() {
        v if v.contains("acros") && (v.contains("+r") || v.contains("red")) => "lc.bw.red-filter",
        v if v.contains("acros") && (v.contains("+ye") || v.contains("yellow")) => "lc.bw.mono-rich",
        v if v.contains("acros") && (v.contains("+g") || v.contains("green")) => "lc.bw.soft",
        v if v.contains("acros") => "lc.bw.mono-rich",
        v if v.contains("monochrome") && (v.contains("+r") || v.contains("red")) => "lc.bw.red-filter",
        v if v.contains("monochrome") && (v.contains("+g") || v.contains("green")) => "lc.bw.soft",
        v if v.contains("monochrome") && (v.contains("+ye") || v.contains("yellow")) => "lc.bw.mono-rich",
        v if v.contains("monochrome") => "lc.mono",
        v if v.contains("sepia") => "lc.bw.sepia",
        v if v.contains("velvia") || v.contains("vivid") => "lc.vivid",
        v if v.contains("astia") || v.contains("soft") => "lc.portrait",
        v if v.contains("classic chrome") => "lc.film.cool-fade",
        v if v.contains("classic negative") || v.contains("classic neg") => "lc.film.faded-slide",
        v if v.contains("reala") => "lc.neutral",
        v if v.contains("nostalgic") => "lc.film.golden-hour",
        v if v.contains("bleach bypass") => "lc.muted.bleached",
        v if v.contains("eterna") => "lc.muted.matte-soft",
        v if v.contains("pro neg") => "lc.portrait",
        v if v.contains("provia") || v.contains("standard") => "lc.color",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_fujifilm_simulation_has_a_preview_profile() {
        for simulation in super::super::settings::FILM_SIMULATIONS {
            assert!(film_profile(simulation).is_some(), "Missing desktop fallback for {simulation}");
        }
    }

    #[test]
    fn hover_preview_changes_a_clone_and_never_the_original_photo() {
        let original = DevelopSettings::default();
        let mut camera = CameraSettings::default();
        camera.set("film_simulation", "Classic Chrome");
        camera.set_number("highlight", -1.0);
        camera.set("grain_strength", "Strong");
        let preview = approximate(&original, &camera).unwrap();
        assert_eq!(preview.profile.id, "lc.film.cool-fade");
        assert_eq!(preview.grain.amount, 55.0);
        assert_eq!(original, DevelopSettings::default());
        assert_eq!(approximate(&original, &CameraSettings::default()), None);
    }

    #[test]
    fn partial_settings_stay_bounded() {
        let original = DevelopSettings::default();
        let mut camera = CameraSettings::default();
        camera.set("film_simulation", "Acros+R");
        camera.set_number("wb_blue", 9.0);
        camera.set_number("highlight", 4.0);
        let preview = approximate(&original, &camera).unwrap();
        assert_eq!(preview.treatment, Treatment::Bw);
        assert_eq!(preview.profile.id, "lc.bw.red-filter");
        assert!(preview.light.highlights <= 100.0);
    }

    #[test]
    fn reference_only_values_do_not_pretend_to_render_a_camera_look() {
        let mut camera = CameraSettings::default();
        camera.set("iso", "Auto up to ISO 6400");
        camera.set("exposure_compensation", "+1/3 to +2/3");
        assert_eq!(approximate(&DevelopSettings::default(), &camera), None);
    }
}
