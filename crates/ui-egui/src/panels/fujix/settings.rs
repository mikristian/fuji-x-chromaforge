//! X100VI camera-recipe values are separate from Chromaforge DevelopSettings.
//! The original source is imperfect PDF-derived text. A missing/unrecognised
//! setting stays *unset*, never fabricated or silently pushed to the camera.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct CameraSettings {
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

impl CameraSettings {
    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or("")
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if value.trim().is_empty() { self.values.remove(key); }
        else { self.values.insert(key.to_string(), value); }
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        self.get(key).parse::<f64>().ok().filter(|n| n.is_finite())
    }

    pub fn set_number(&mut self, key: &str, value: f64) {
        let value = ((value * 2.0).round() / 2.0).clamp(-20.0, 20.0);
        self.set(key, if value.fract() == 0.0 { format!("{value:.0}") } else { format!("{value:.1}") });
    }

    /// Parse only explicitly labelled camera parameters. Entries with
    /// inconsistent OCR layouts can be corrected in the controls; references
    /// and unsupported lines remain available in the advanced original text.
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for (key, aliases) in [
            ("film_simulation", &["film simulation", "simulation:"][..]),
            ("dynamic_range", &["dynamic range", "d-range priority"][..]),
            ("white_balance", &["white balance", "wb / color temperature", "wb:"][..]),
            ("iso", &["iso:", "iso\n"][..]),
            ("exposure_compensation", &["exposure compensation", "exp. compensation", "ev compensation"][..]),
        ] {
            if let Some(v) = chunk(text, aliases) {
                let value = clean_piece(&v);
                if !value.is_empty() { out.set(key, value); }
            }
        }

        // The PDF catalog often prints a camera simulation on its own line
        // under the recipe title: "Classic Chrome", "Astia", "PRO Neg. Std"
        // or "Acros (+Y, +R, +G)". Recognize only a *whole* labeled line or
        // a known short camera alias, not words inside creative recipe names
        // such as "dramatic classic chrome" or "Kodak Portra 400".
        if out.get("film_simulation").is_empty() {
            for (position, line) in text.lines().take(9).enumerate() {
                if let Some(simulation) = standalone_simulation(line, position == 0) {
                    out.set("film_simulation", simulation); break;
                }
            }
        }
        for (key, aliases) in [
            ("highlight", &["highlight tone", "highlights:", "highlight:", "tone curve:"][..]),
            ("shadow", &["shadow tone", "shadows:", "shadow:"][..]),
            ("color", &["color:", "colour:"][..]),
            ("sharpness", &["sharpness", "sharpening"][..]),
            ("noise_reduction", &["high iso nr", "noise reduction", "iso noise reduction"][..]),
            ("clarity", &["clarity"][..]),
        ] {
            if let Some(v) = chunk(text, aliases)
                && let Some(n) = first_number(&v) {
                out.set_number(key, n);
            }
        }
        // Tone Curve: H-1, S-1 is a common X100VI setting notation.
        if let Some(curve) = chunk(text, &["tone curve:"]) {
            if let Some(n) = number_near_marker(&curve, 'h') { out.set_number("highlight", n); }
            if let Some(n) = number_near_marker(&curve, 's') { out.set_number("shadow", n); }
        }

        let grain = chunk(text, &["grain effect", "grain effect / grain size", "grain:"]);
        if let Some(v) = grain {
            if let Some(k) = tri_choice(&v) { out.set("grain_strength", k); }
            if let Some(size) = grain_size(&v) { out.set("grain_size", size); }
        }
        if out.get("grain_size").is_empty()
            && let Some(v) = chunk(text, &["grain size", "grain effect-size"]) {
            if let Some(size) = grain_size(&v) { out.set("grain_size", size); }
        }
        if let Some(v) = chunk(text, &["color chrome effect / fx blue", "colour chrome effect / fx blue"]) {
            let found: Vec<_> = v.split(['/', ',']).filter_map(tri_choice).collect();
            if let Some(one) = found.first() { out.set("color_chrome", *one); }
            if let Some(two) = found.get(1) { out.set("color_chrome_blue", *two); }
        }
        if out.get("color_chrome").is_empty()
            && let Some(v) = chunk(text, &["color chrome effect:", "colour chrome effect:", "color chrome effect\n", "colour chrome effect\n"]) {
            if let Some(value) = tri_choice(&v) { out.set("color_chrome", value); }
        }
        if out.get("color_chrome_blue").is_empty()
            && let Some(v) = chunk(text, &["color chrome fx blue", "colour chrome fx blue", "color chrome effect blue", "colour chrome blue"]) {
            if let Some(value) = tri_choice(&v) { out.set("color_chrome_blue", value); }
        }
        // Red/blue WB shifts may be written as +3 Red, Red -3, R6, or -R2;
        // do not infer shifts if the recipe has no explicit WB field.
        if let Some(wb) = chunk(text, &["white balance", "wb shift", "wb / color temperature", "wb:"]) {
            if let Some(red) = shift(&wb, 'r', "red") { out.set_number("wb_red", red); }
            if let Some(blue) = shift(&wb, 'b', "blue") { out.set_number("wb_blue", blue); }
        }
        if out.get("wb_red").is_empty() || out.get("wb_blue").is_empty() {
            if let Some(wb) = chunk(text, &["wb shift"]) {
                if let Some(v) = shift(&wb, 'r', "red") { out.set_number("wb_red", v); }
                if let Some(v) = shift(&wb, 'b', "blue") { out.set_number("wb_blue", v); }
            }
        }
        if let Some(wb) = out.values.get_mut("white_balance") {
            // Keep a useful camera mode rather than displaying the shift/DR
            // values as though they were part of the mode itself.
            *wb = wb.split(',').next().unwrap_or("").trim().to_string();
        }
        // A colour temperature is one camera WB mode with a separate Kelvin
        // value, not a second competing edit of the same 'white_balance' key.
        let wb = out.get("white_balance").to_string();
        if wb.trim_end().to_ascii_uppercase().ends_with('K')
            && wb.trim_end_matches(|c: char| c == 'K' || c == 'k').trim().parse::<u16>().is_ok() {
            out.set("white_balance_kelvin", wb);
            out.set("white_balance", "Kelvin");
        }
        out
    }

    /// Camera-side settings as a separate draft. Never claim this can be
    /// rendered as the same colors by Chromaforge or is ready for PTP write.
    pub fn as_camera_text(&self, name: &str) -> String {
        let mut lines = vec![name.to_owned()];
        for (key, label) in [
            ("film_simulation", "Film Simulation"), ("dynamic_range", "Dynamic Range"),
            ("white_balance", "White Balance"), ("white_balance_kelvin", "Color Temperature"), ("wb_red", "WB Shift Red"),
            ("wb_blue", "WB Shift Blue"), ("highlight", "Highlight"),
            ("shadow", "Shadow"), ("color", "Color"), ("sharpness", "Sharpness"),
            ("noise_reduction", "High ISO NR"), ("clarity", "Clarity"),
            ("grain_strength", "Grain Effect"), ("grain_size", "Grain Size"),
            ("color_chrome", "Color Chrome Effect"), ("color_chrome_blue", "Color Chrome FX Blue"),
            ("smooth_skin", "Smooth Skin Effect"), ("d_range_priority", "D-Range Priority"),
            ("mono_warm_cool", "Monochromatic Warm/Cool"), ("mono_magenta_green", "Monochromatic Magenta/Green"),
            ("iso", "ISO (reference)"), ("exposure_compensation", "Exposure Compensation (reference)"),
        ] {
            let v = self.get(key);
            if !v.is_empty() { lines.push(format!("{label}: {v}")); }
        }
        lines.join("\n")
    }
}

pub(super) const FILM_SIMULATIONS: &[&str] = &[
    "Provia/Standard", "Velvia/Vivid", "Astia/Soft", "Classic Chrome", "Classic Negative",
    "Reala Ace", "Nostalgic Neg.", "Eterna", "Eterna Bleach Bypass", "Pro Neg. Hi",
    "Pro Neg. Std", "Acros", "Acros+Ye", "Acros+R", "Acros+G", "Monochrome",
    "Monochrome+Ye", "Monochrome+R", "Monochrome+G", "Sepia",
];

fn standalone_simulation(source: &str, is_title: bool) -> Option<&'static str> {
    let value = source.trim().trim_start_matches(['•', '-', '*']).trim();
    let lower = value.to_ascii_lowercase();
    // OCR can separate each letter in "F i l m  s i m u l a t i o n".
    if lower.replace(' ', "").starts_with("filmsimulation:") {
        return standalone_simulation(value.split_once(':')?.1, false);
    }
    for &name in FILM_SIMULATIONS {
        if value.eq_ignore_ascii_case(name) { return Some(name); }
    }
    // An unlabelled title isn't evidence for a simulation; the aliases below
    // are only valid in the subsequent camera-parameter lines.
    if is_title { return None; }
    let alias = match lower.as_str() {
        "provia" | "provia/std" => "Provia/Standard",
        "velvia" => "Velvia/Vivid",
        "astia" => "Astia/Soft",
        "classic neg" => "Classic Negative",
        "nostalgic neg" => "Nostalgic Neg.",
        "pro neg std" | "pro neg. std" => "Pro Neg. Std",
        "pro neg hi" | "pro neg. hi" => "Pro Neg. Hi",
        v if v.starts_with("acros (") => "Acros", // filters ambiguous; no invented choice
        _ => return None,
    };
    Some(alias)
}

const FIELD_BOUNDARIES: &[&str] = &[
    "film simulation", "simulation:", "dynamic range", "d-range priority",
    "grain effect", "grain size", "grain:", "color chrome effect", "colour chrome effect",
    "color chrome fx blue", "colour chrome fx blue", "colour chrome blue", "white balance",
    "wb / color temperature", "wb shift", "wb:", "tone curve", "highlight", "shadow",
    "color:", "colour:", "sharpness", "sharpening", "high iso nr", "noise reduction",
    "iso noise reduction", "clarity", "exp. compensation", "exposure compensation",
    "ev compensation", "iso:", "iso\n", "color chrome effect blue",
];

/// Source segmentation avoids overwriting an adjacent parameter when a PDF
/// places several fields on the same line.
fn chunk(source: &str, aliases: &[&str]) -> Option<String> {
    let lower = source.to_ascii_lowercase();
    let found = aliases.iter().filter_map(|alias| {
        let needle = alias.to_ascii_lowercase();
        lower.match_indices(&needle).find(|(pos, _)| {
            *pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphabetic()
        }).map(|(pos, _)| (pos, pos + needle.len()))
    }).min_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)))?;
    let (_, begin) = found;
    let end = FIELD_BOUNDARIES.iter().filter_map(|label| {
        let needle = label.to_ascii_lowercase();
        lower[begin..].match_indices(&needle).find(|(relative, _)| {
            let absolute = begin + relative;
            absolute == 0 || !lower.as_bytes()[absolute - 1].is_ascii_alphabetic()
        }).map(|(relative, _)| begin + relative)
    }).min().unwrap_or(source.len());
    // Do not strip '-' here: the majority of Fujifilm highlight, color, NR
    // and clarity recipes use meaningful negative signed values (-4, -2…).
    let tail = source.get(begin..end)?.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '/' | '•'));
    let value = tail.trim().trim_start_matches('●').trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn clean_piece(value: &str) -> String {
    let text = value.lines().map(str::trim).find(|s| !s.is_empty() && *s != "●" && *s != "•").unwrap_or("");
    text.chars().take(100).collect::<String>().trim().trim_matches(':').trim().to_owned()
}

fn first_number(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() && !matches!(bytes[start], b'+' | b'-') { continue; }
        if matches!(bytes[start], b'+' | b'-') && !bytes.get(start+1).is_some_and(u8::is_ascii_digit) { continue; }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || matches!(bytes[end], b'+' | b'-' | b'.')) { end += 1; }
        if let Some(v) = value.get(start..end).and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite() && v.abs() <= 20.0) { return Some(v); }
    }
    None
}

fn number_near_marker(value: &str, marker: char) -> Option<f64> {
    let at = value.to_ascii_lowercase().find(marker)?;
    first_number(value.get(at+1..)?.trim_start_matches([':', ' ', '=']))
}

fn tri_choice(value: &str) -> Option<&'static str> {
    let v = value.to_ascii_lowercase();
    for (word, label) in [("strong", "Strong"), ("weak", "Weak"), ("off", "Off")] {
        if v.contains(word) { return Some(label); }
    }
    None
}

fn grain_size(value: &str) -> Option<&'static str> {
    let v = value.to_ascii_lowercase();
    if v.contains("large") { Some("Large") }
    else if v.contains("small") { Some("Small") }
    else { None }
}

fn shift(input: &str, short: char, long: &str) -> Option<f64> {
    let lower = input.to_ascii_lowercase();
    let mut loc = lower.find(long).or_else(|| {
        lower.match_indices(short).map(|(p, _)| p).find(|p| {
            let pre = &lower[..*p];
            !pre.ends_with(|c: char| c.is_ascii_alphabetic())
        })
    })?;
    if lower.get(loc..)?.starts_with(long) { loc += long.len(); } else { loc += 1; }
    // Red -3, Red: +2, +4 Red, R6, -R2 and B-5 are common formats.
    if let Some(right) = lower.get(loc..) {
        let right = right.trim_start_matches([':', ' ', '=']);
        let number_at_start = right.as_bytes().first().is_some_and(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-'));
        if number_at_start && let Some(num) = first_number(right) {
            // -R2 and +B5 put their sign before the letter.
            let preceding = lower.get(..loc.saturating_sub(1)).unwrap_or("").chars().next_back();
            let num = if preceding == Some('-') && num >= 0.0 { -num } else { num };
            return Some(num);
        }
    }
    let left = lower.get(..loc)?.trim_end();
    if let Some(num) = left.split(|c: char| matches!(c, ',' | '&' | '/' | '\n')).next_back().and_then(first_number) {
        return Some(num);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_fujifilm_pdf_recipe_and_grain() {
        let raw = "Film Simulation: Classic Chrome\nGrain Effect: Weak, Small Color Chrome Effect: Strong Color Chrome FX Blue:\nWeak White Balance: Auto, +3 Red, -5 Blue Dynamic Range: Auto Tone Curve: H-1, S-1\nColor: +2\nSharpness: -2\nHigh ISO NR: -4\nClarity: 0";
        let s = CameraSettings::parse(raw);
        assert_eq!(s.get("film_simulation"), "Classic Chrome");
        assert_eq!(s.get("grain_strength"), "Weak");
        assert_eq!(s.get("grain_size"), "Small");
        assert_eq!(s.get("color_chrome"), "Strong");
        assert_eq!(s.get("color_chrome_blue"), "Weak");
        assert_eq!(s.number("wb_red"), Some(3.0));
        assert_eq!(s.number("wb_blue"), Some(-5.0));
        assert_eq!(s.number("highlight"), Some(-1.0));
        assert_eq!(s.number("shadow"), Some(-1.0));
        assert_eq!(s.number("color"), Some(2.0));
        assert_eq!(s.number("noise_reduction"), Some(-4.0));
    }

    #[test]
    fn recovers_standalone_film_simulations_without_guessing_from_titles() {
        for (recipe, expected) in [
            ("Kodachrome 64\n\nClassic Chrome\nDynamic Range: DR200", "Classic Chrome"),
            ("Fuji Color NPH\nProvia\nColor: +2", "Provia/Standard"),
            ("Vintage print\nPRO Neg. Std\nShadow: +1", "Pro Neg. Std"),
            ("Kodak Tri-X 400\nAcros (+Y, +R, +G)\nClarity: 0", "Acros"),
            ("Portrait film\n\nAstia\nSharpness: -2", "Astia/Soft"),
            ("•F i l m  s i m u l a t i o n: Classic Chrome\nColor: +2", "Classic Chrome"),
        ] {
            assert_eq!(CameraSettings::parse(recipe).get("film_simulation"), expected, "{recipe}");
        }
        assert_eq!(CameraSettings::parse("Dramatic Classic Chrome\nSharpness: 0").get("film_simulation"), "");
        assert_eq!(CameraSettings::parse("Dreamy Negative Recipe: CLASSIC\nNotes: handheld").get("film_simulation"), "");
    }

    #[test]
    fn parses_wb_red_before_and_after_number() {
        for (text, r, b) in [
            ("White Balance: Daylight, +4 Red, -5 Blue", 4.0, -5.0),
            ("WB: Auto White Priority, Red -3 & Blue +3", -3.0, 3.0),
            ("WB / Color Temperature: 6100K, -R2 +B5", -2.0, 5.0),
        ] {
            let s = CameraSettings::parse(text);
            assert_eq!(s.number("wb_red"), Some(r), "{text}");
            assert_eq!(s.number("wb_blue"), Some(b), "{text}");
        }
    }

    #[test]
    fn kelvin_white_balance_has_one_mode_and_separate_temperature() {
        let s = CameraSettings::parse("White Balance: 6300K, +2 Red, -3 Blue");
        assert_eq!(s.get("white_balance"), "Kelvin");
        assert_eq!(s.get("white_balance_kelvin"), "6300K");
        assert_eq!(s.number("wb_red"), Some(2.0));
        assert_eq!(s.number("wb_blue"), Some(-3.0));
        assert!(s.as_camera_text("Test").contains("Color Temperature: 6300K"));
    }

    #[test]
    fn original_unknown_settings_not_invented() {
        let s = CameraSettings::parse("Film Simulation: Reala Ace\nNotes: handheld and tripod");
        assert_eq!(s.get("film_simulation"), "Reala Ace");
        assert_eq!(s.number("grain_strength"), None);
        assert_eq!(s.number("highlight"), None);
        assert!(!s.as_camera_text("My Recipe").contains("High ISO NR"));
    }

    #[test]
    fn negative_fujifilm_values_are_never_rewritten_as_positive() {
        let raw = "Color: -4\nSharpness: -2\nHigh ISO NR: -4\nClarity: -5\nHighlight: -1.5\nShadow: -2";
        let s = CameraSettings::parse(raw);
        for (key, number) in [("color", -4.0), ("sharpness", -2.0),
            ("noise_reduction", -4.0), ("clarity", -5.0),
            ("highlight", -1.5), ("shadow", -2.0)] {
            assert_eq!(s.number(key), Some(number), "{key}: negative tone is preserved");
        }
    }
}
