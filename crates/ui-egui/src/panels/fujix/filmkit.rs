//! Import a single FilmKit `.filmkit` / JSON preset as an editable local
//! Fujifilm camera-recipe draft. FilmKit is an MIT-licensed reference project
//! by eggricesoy: https://github.com/eggricesoy/filmkit
//! This code is an independent format adapter and never writes camera PTP
//! properties. Numeric simulation codes follow FilmKit's public enums;
//! real X100VI write correctness is not inferred from those codes.

use super::settings::CameraSettings;

/// A preserved, editable local draft. The raw source JSON can be retained
/// separately in reference notes; unmapped fields are never silently sent.
pub(super) fn import(data: &[u8], fallback_name: &str) -> Result<(String, CameraSettings), String> {
    if data.len() > 1_048_576 { return Err("FilmKit file exceeds 1 MiB".into()); }
    let value: serde_json::Value = serde_json::from_slice(data)
        .map_err(|e| format!("Not valid FilmKit JSON: {e}"))?;
    let fields = value.get("values").and_then(serde_json::Value::as_object)
        .ok_or("FilmKit preset must have a numeric values object")?;
    let name = value.get("name").and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty()).unwrap_or(fallback_name).trim();
    if name.is_empty() || name.chars().count() > 120 { return Err("FilmKit preset name must be between 1 and 120 characters".into()); }
    let mut out = CameraSettings::default();

    // The first-party FujiX name list is canonical for its own editor.
    const SIMULATIONS: &[&str; 21] = &[
        "", "Provia/Standard", "Velvia/Vivid", "Astia/Soft", "Pro Neg. Hi",
        "Pro Neg. Std", "Monochrome", "Monochrome+Ye", "Monochrome+R",
        "Monochrome+G", "Sepia", "Classic Chrome", "Acros", "Acros+Ye",
        "Acros+R", "Acros+G", "Eterna", "Classic Negative",
        "Eterna Bleach Bypass", "Nostalgic Neg.", "Reala Ace",
    ];
    if let Some(sim) = number(fields, "filmSimulation")? {
        if sim.fract() != 0.0 || !(0.0..=20.0).contains(&sim) {
            return Err("Invalid FilmKit film simulation".into());
        }
        let index = sim as usize;
        let name = SIMULATIONS.get(index).ok_or("Unknown FilmKit film simulation; no camera setting was changed")?;
        if !name.is_empty() { out.set("film_simulation", *name); }
    }
    if let Some(dr) = number(fields, "dynamicRange")? {
        let v = match dr as i64 {
            0 => "Auto", 1 => "DR100", 2 => "DR200", 3 => "DR400",
            _ => return Err("Unknown FilmKit dynamic range value".into()),
        };
        out.set("dynamic_range", v);
    }
    if let Some(v) = number(fields, "grainEffect")? {
        let (strength, size) = match v as i64 {
            0 => ("Off", ""), 2 => ("Weak", "Small"), 3 => ("Strong", "Small"),
            0x102 => ("Weak", "Large"), 0x103 => ("Strong", "Large"),
            _ => return Err("Unknown FilmKit grain strength/size value".into()),
        };
        out.set("grain_strength", strength);
        if !size.is_empty() { out.set("grain_size", size); }
    }
    for (src, dst) in [
        ("smoothSkin", "smooth_skin"), ("colorChrome", "color_chrome"),
        ("colorChromeFxBlue", "color_chrome_blue"), ("dRangePriority", "d_range_priority"),
    ] {
        if let Some(n) = number(fields, src)? {
            let name = if src == "dRangePriority" {
                match n as i64 { 0 => "Off", 1 => "Auto", 2 => "Weak", 3 => "Strong",
                    _ => return Err(format!("Unknown FilmKit {src} setting")) }
            } else { match n as i64 { 0 => "Off", 1 => "Weak", 2 => "Strong",
                _ => return Err(format!("Unknown FilmKit {src} setting")) } };
            out.set(dst, name);
        }
    }
    if let Some(wb) = number(fields, "whiteBalance")? {
        let mode = match wb as i64 {
            0 => "As Shot", 2 => "Auto", 4 => "Daylight", 6 => "Incandescent",
            8 => "Underwater", 0x8001 => "Fluorescent 1", 0x8002 => "Fluorescent 2",
            0x8003 => "Fluorescent 3", 0x8006 => "Shade", 0x8007 => "Kelvin",
            0x8021 => "Auto Ambience Priority",
            _ => return Err("Unknown FilmKit white-balance mode".into()),
        };
        out.set("white_balance", mode);
        if mode == "Kelvin" {
            if let Some(value) = number(fields, "wbColorTemp")? {
                if !(2000.0..=10000.0).contains(&value) { return Err("FilmKit Kelvin value outside 2000–10000K".into()); }
                out.set("white_balance_kelvin", format!("{}K", value as u32));
            }
        }
    }
    for (source, target, lo, hi) in [
        ("wbShiftR", "wb_red", -9.0, 9.0), ("wbShiftB", "wb_blue", -9.0, 9.0),
        ("highlightTone", "highlight", -2.0, 4.0), ("shadowTone", "shadow", -2.0, 4.0),
        ("color", "color", -4.0, 4.0), ("sharpness", "sharpness", -4.0, 4.0),
        ("noiseReduction", "noise_reduction", -4.0, 4.0), ("clarity", "clarity", -5.0, 5.0),
        ("monoWC", "mono_warm_cool", -9.0, 9.0),
        ("monoMG", "mono_magenta_green", -9.0, 9.0),
    ] {
        if let Some(n) = number(fields, source)? {
            if !(lo..=hi).contains(&n) { return Err(format!("FilmKit {source} is outside the X100VI editing range")); }
            out.set_number(target, n);
        }
    }
    if let Some(exposure) = number(fields, "exposure")? && exposure != 0.0 {
        if !(-5.0..=5.0).contains(&exposure) { return Err("FilmKit exposure outside expected range".into()); }
        out.set("exposure_compensation", format!("{exposure:+} (reference only)"));
    }
    if out.values.is_empty() { return Err("FilmKit preset contains no recognizable FujiX camera parameters".into()); }
    Ok((name.into(), out))
}

fn number(fields: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<Option<f64>, String> {
    let Some(value) = fields.get(key) else { return Ok(None); };
    let Some(value) = value.as_f64().filter(|v| v.is_finite()) else {
        return Err(format!("FilmKit {key} must be a finite number"));
    };
    if matches!(key, "filmSimulation" | "dynamicRange" | "grainEffect" | "smoothSkin" | "colorChrome" | "colorChromeFxBlue" | "dRangePriority" | "whiteBalance" | "wbColorTemp") && value.fract() != 0.0 {
        return Err(format!("FilmKit {key} requires an integer code"));
    }
    Ok(Some(value))
}

/// Write the compatible numeric subset of a locally edited FujiX recipe in
/// FilmKit's documented portable preset schema. It is never a camera backup,
/// and deliberately does not invent missing/default camera parameters.
pub(super) fn export(name: &str, camera: &CameraSettings) -> Result<Vec<u8>, String> {
    use serde_json::{json, Map, Value};
    if name.trim().is_empty() || name.chars().count() > 120 {
        return Err("FilmKit export needs a recipe name of 1–120 characters".into());
    }
    let mut values = Map::new();
    let simulations = [
        "", "Provia/Standard", "Velvia/Vivid", "Astia/Soft", "Pro Neg. Hi",
        "Pro Neg. Std", "Monochrome", "Monochrome+Ye", "Monochrome+R",
        "Monochrome+G", "Sepia", "Classic Chrome", "Acros", "Acros+Ye",
        "Acros+R", "Acros+G", "Eterna", "Classic Negative",
        "Eterna Bleach Bypass", "Nostalgic Neg.", "Reala Ace",
    ];
    let simulation = camera.get("film_simulation");
    if !simulation.is_empty() {
        let Some(index) = simulations.iter().position(|label| label.eq_ignore_ascii_case(simulation)) else {
            return Err(format!("Cannot encode unsupported FilmKit simulation: {simulation}"));
        };
        values.insert("filmSimulation".into(), json!(index));
    }
    for (source, destination, labels) in [
        ("dynamic_range", "dynamicRange", &["Auto", "DR100", "DR200", "DR400"][..]),
        ("smooth_skin", "smoothSkin", &["Off", "Weak", "Strong"][..]),
        ("color_chrome", "colorChrome", &["Off", "Weak", "Strong"][..]),
        ("color_chrome_blue", "colorChromeFxBlue", &["Off", "Weak", "Strong"][..]),
        ("d_range_priority", "dRangePriority", &["Off", "Auto", "Weak", "Strong"][..]),
    ] {
        let value = camera.get(source);
        if value.is_empty() { continue; }
        let Some(index) = labels.iter().position(|s| s.eq_ignore_ascii_case(value)) else {
            return Err(format!("Cannot encode unsupported FilmKit {source}: {value}"));
        };
        values.insert(destination.into(), json!(index));
    }
    let grain = camera.get("grain_strength");
    let grain_size = camera.get("grain_size");
    if !grain.is_empty() {
        let code = match (grain, grain_size) {
            ("Off", "" | "Small" | "Large") => 0,
            ("Weak", "" | "Small") => 2,
            ("Strong", "" | "Small") => 3,
            ("Weak", "Large") => 0x102,
            ("Strong", "Large") => 0x103,
            _ => return Err("Unknown FilmKit grain strength/size combination".into()),
        };
        values.insert("grainEffect".into(), json!(code));
    } else if !grain_size.is_empty() {
        return Err("Set grain strength before exporting a FilmKit grain size".into());
    }
    let wb = camera.get("white_balance");
    if !wb.is_empty() {
        let code = match wb {
            "As Shot" => 0, "Auto" => 2, "Daylight" => 4, "Incandescent" => 6,
            "Underwater" => 8, "Fluorescent 1" => 0x8001, "Fluorescent 2" => 0x8002,
            "Fluorescent 3" => 0x8003, "Shade" => 0x8006, "Kelvin" => 0x8007,
            "Auto Ambience Priority" => 0x8021,
            _ => return Err(format!("Cannot encode unsupported FilmKit white-balance mode: {wb}")),
        };
        values.insert("whiteBalance".into(), json!(code));
    }
    let kelvin = camera.get("white_balance_kelvin");
    if !kelvin.is_empty() {
        if wb != "Kelvin" { return Err("Select Kelvin white balance before exporting its temperature".into()); }
        let temperature: u16 = kelvin.trim_end_matches(['K', 'k']).parse()
            .map_err(|_| "FilmKit Kelvin value must be a number, e.g. 6300K")?;
        if !(2000..=10000).contains(&temperature) { return Err("FilmKit Kelvin value outside 2000–10000K".into()); }
        values.insert("wbColorTemp".into(), json!(temperature));
    }
    for (source, dest, lo, hi) in [
        ("wb_red", "wbShiftR", -9.0, 9.0), ("wb_blue", "wbShiftB", -9.0, 9.0),
        ("highlight", "highlightTone", -2.0, 4.0), ("shadow", "shadowTone", -2.0, 4.0),
        ("color", "color", -4.0, 4.0), ("sharpness", "sharpness", -4.0, 4.0),
        ("noise_reduction", "noiseReduction", -4.0, 4.0), ("clarity", "clarity", -5.0, 5.0),
        ("mono_warm_cool", "monoWC", -9.0, 9.0),
        ("mono_magenta_green", "monoMG", -9.0, 9.0),
    ] {
        let raw = camera.get(source);
        if raw.is_empty() { continue; }
        let n = camera.number(source).ok_or_else(|| format!("Invalid {source} numeric setting"))?;
        if !(lo..=hi).contains(&n) { return Err(format!("{source} is outside FilmKit's expected X100VI adjustment range")); }
        values.insert(dest.into(), json!(n));
    }
    // Exposure compensation and ISO are kept in FujiX's own reference fields:
    // importing into FilmKit would otherwise apply assumed camera values.
    if values.is_empty() { return Err("This recipe has no FilmKit-compatible camera settings".into()); }
    let mut packet = json!({ "filmkit": 1, "name": name, "values": values });
    let unmapped: Map<String, Value> = camera.values.iter()
        .filter(|(key, _)| !matches!(key.as_str(),
            "film_simulation" | "dynamic_range" | "smooth_skin" | "color_chrome" | "color_chrome_blue" |
            "d_range_priority" | "grain_strength" | "grain_size" | "white_balance" | "white_balance_kelvin" |
            "wb_red" | "wb_blue" | "highlight" | "shadow" | "color" | "sharpness" |
            "noise_reduction" | "clarity" | "mono_warm_cool" | "mono_magenta_green"))
        .map(|(key, value)| (key.clone(), Value::String(value.clone()))).collect();
    if !unmapped.is_empty() { packet["_fujixReferenceOnly"] = Value::Object(unmapped); }
    serde_json::to_vec_pretty(&packet).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filmkit_preset_imports_all_supported_fields() {
        let src = br#"{"name":"Cinema Test","values":{"filmSimulation":16,"dynamicRange":3,"grainEffect":259,"whiteBalance":32775,"wbColorTemp":6300,"wbShiftR":3,"wbShiftB":-5,"highlightTone":-1,"shadowTone":2,"colorChrome":2,"colorChromeFxBlue":1,"clarity":-3}}"#;
        let (name, values) = import(src, "fallback").unwrap();
        assert_eq!(name, "Cinema Test");
        assert_eq!(values.get("film_simulation"), "Eterna");
        assert_eq!(values.get("grain_strength"), "Strong");
        assert_eq!(values.get("grain_size"), "Large");
        assert_eq!(values.get("white_balance_kelvin"), "6300K");
        assert_eq!(values.get("color_chrome"), "Strong");
        assert_eq!(values.number("wb_blue"), Some(-5.0));
    }

    #[test]
    fn arbitrary_json_and_unsupported_camera_codes_fail_closed() {
        assert!(import(br#"{"name":"x","other":true}"#, "x").is_err());
        assert!(import(br#"{"values":{"filmSimulation":2000}}"#, "x").is_err());
        assert!(import(br#"{"values":{"filmSimulation":1.5}}"#, "x").is_err());
        assert!(import(br#"{"values":{"wbShiftR":100000}}"#, "x").is_err());
    }

    #[test]
    fn export_roundtrips_known_numeric_parameters_and_keeps_reference_notes() {
        let source = br#"{"name":"Cinema Test","values":{"filmSimulation":16,"dynamicRange":3,"grainEffect":259,"whiteBalance":32775,"wbColorTemp":6300,"wbShiftR":3,"wbShiftB":-5,"colorChrome":2}}"#;
        let (name, mut settings) = import(source, "Unknown").unwrap();
        settings.set("iso", "Auto ISO 6400");
        let output = export(&name, &settings).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(doc["filmkit"], 1);
        assert_eq!(doc["values"]["filmSimulation"], 16);
        assert_eq!(doc["values"]["grainEffect"], 259);
        assert_eq!(doc["_fujixReferenceOnly"]["iso"], "Auto ISO 6400");
        let (_, restored) = import(&output, "Unknown").unwrap();
        assert_eq!(restored.get("film_simulation"), "Eterna");
        assert_eq!(restored.get("grain_size"), "Large");
        assert_eq!(restored.get("white_balance_kelvin"), "6300K");
    }
}
