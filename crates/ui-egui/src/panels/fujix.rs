//! Native FujiX camera-recipe panels inside Chromaforge's Rust/egui editor.
//! The source PDF recipes are read-only; user edits are separately saved.
//! Camera discovery is intentionally read-only until actual USB PTP support is
//! validated on this model and backup/restore semantics are proved.

use egui::{Align2, Color32, Rect, RichText, Sense, pos2, vec2};
use std::collections::BTreeMap;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use lcc_develop::{ControlSpec, DevelopSettings, Section, Track};
use lcc_catalog::PhotoId;
use crate::{LccApp, state::RightPanel, theme::Tokens};
use crate::module::{ModuleId, PanelId};
use crate::icons::{Icon, paint};
use crate::widgets::{register, section_header, divider, slider};
mod settings;
mod preview;
mod filmkit;
#[cfg(not(target_arch = "wasm32"))]
mod preflight;
use settings::{CameraSettings, FILM_SIMULATIONS};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct UsbProbe {
    busy: bool,
    detail: String,
    verified: bool,
    active_slot: Option<String>,
    raw_mode_detail: String,
    raw_mode_verified: bool,
    raf_preflight_detail: String,
    raf_preflight_verified: bool,
    full_current_detail: String,
    full_current_verified: bool,
}

#[derive(Clone, Deserialize)]
struct RecipeSource { backups: Vec<Backup>, #[serde(rename = "recipeCount")] recipe_count: usize }
#[derive(Clone, Deserialize)]
struct Backup { #[serde(rename = "backupNumber")] backup_number: u32, slots: Vec<OriginalRecipe> }
#[derive(Clone, Deserialize)]
struct OriginalRecipe {
    id: String,
    name: String,
    #[serde(rename = "rawText")]
    raw_text: String,
    #[serde(default)]
    page: u32,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct RecipeDraft {
    id: String,
    name: String,
    text: String,
    /// Explicitly edited X100VI camera parameters. Original source text is
    /// kept for provenance, and old drafts without this field still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera: Option<CameraSettings>,
    /// Optional *offline* custom-backup slot. This is never a physical
    /// camera C-slot mapping, and old LightCraft drafts load without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_slot: Option<LocalBackupSlot>,
    /// Human name for the *offline* backup bank. Stored on every slot in the
    /// existing atomic recipe-drafts.json so older drafts stay compatible and
    /// no split-metadata write can leave a renamed backup half-saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backup_label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LocalBackupSlot { backup: u32, slot: u8 }

/// Portable user-created backup, deliberately not an X RAW STUDIO camera
/// settings file. Camera slot writes require separate verified protocols.
#[derive(Clone, Serialize, Deserialize)]
struct OfflineBackupPackage {
    kind: String,
    version: u32,
    name: String,
    slots: Vec<OfflineBackupSlot>,
}

#[derive(Clone, Serialize, Deserialize)]
struct OfflineBackupSlot {
    slot: u8,
    name: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    camera: CameraSettings,
}

const OFFLINE_BACKUP_KIND: &str = "fujix-chromaforge-offline-backup";

impl OfflineBackupPackage {
    fn validate(&self) -> Result<(), String> {
        if self.kind != OFFLINE_BACKUP_KIND || self.version != 1 {
            return Err("Not a supported FujiX Chromaforge offline backup (v1)".into());
        }
        if self.name.is_empty() || self.name.chars().count() > 80 || self.slots.len() != 7 {
            return Err("A backup needs a name (1–80 characters) and exactly seven C-slots".into());
        }
        let mut seen = [false; 7];
        for slot in &self.slots {
            if !(1..=7).contains(&slot.slot) || seen[usize::from(slot.slot - 1)] {
                return Err("Backup slots must be unique C1–C7 values".into());
            }
            seen[usize::from(slot.slot - 1)] = true;
            if slot.name.trim().is_empty() || slot.name.chars().count() > 120
                || slot.text.len() > 16000 || slot.camera.values.len() > 64
                || slot.camera.values.iter().any(|(key, value)| key.len() > 80 || value.len() > 512) {
                return Err(format!("C{} has invalid or oversized recipe data", slot.slot));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FujiState {
    search: String,
    selected: Option<String>,
    drafts: Vec<RecipeDraft>,
    original: Vec<(u32, OriginalRecipe)>,
    /// Shared immutable parsed source. Hovering through seven slots avoids
    /// scanning and reparsing PDF text on every GUI frame.
    original_camera: Arc<BTreeMap<String, CameraSettings>>,
    name: String,
    text: String,
    camera: CameraSettings,
    editing: bool,
    message: String,
    camera_slot: usize,
    new_backup_name: String,
    rename_backup: Option<u32>,
    rename_buffer: String,
    confirm_delete_backup: Option<u32>,
    confirm_delete_recipe: Option<String>,
    copy_target_backup: u32,
    copy_target_slot: u8,
    pending_slot_overwrite: Option<LocalBackupSlot>,
    import_backup_path: String,
    import_filmkit_path: String,
    raf_preflight_path: String,
    #[cfg(not(target_arch = "wasm32"))]
    probe: Arc<Mutex<UsbProbe>>,
}

impl FujiState {
    fn load() -> Self {
        let parsed: Result<RecipeSource, _> = serde_json::from_str(include_str!("fujix-recipes.json"));
        let (original, message) = match parsed {
            Ok(src) => {
                (src.backups.into_iter().flat_map(|b| b.slots.into_iter().map(move |r| (b.backup_number, r))).collect(), String::new())
            }

            Err(e) => (Vec::new(), format!("FujiX source catalog error: {e}")),
        };
        // Preserve any prior custom FujiX LightCraft drafts as an optional
        // read-only migration source. Chromaforge always writes to its own
        // separate user-library directory, never back to LightCraft.
        let new = draft_path();
        let path = new.as_ref().filter(|path| path.exists()).cloned().or_else(legacy_draft_path);
        let drafts = path.and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|s| serde_json::from_str::<Vec<RecipeDraft>>(&s).ok()).unwrap_or_default();
        let original_camera = Arc::new(original.iter()
            .map(|(_, r)| (r.id.clone(), CameraSettings::parse(&r.raw_text)))
            .collect());
        Self { original, original_camera, drafts, message, camera_slot: 1, ..Self::default() }
    }

    fn select(&mut self, id: &str) {
        if let Some(d) = self.drafts.iter().find(|d| d.id == id) {
            (self.name, self.text) = (d.name.clone(), d.text.clone());
            self.camera = d.camera.clone().unwrap_or_else(|| CameraSettings::parse(&self.text));
        } else if let Some((_, r)) = self.original.iter().find(|(_, r)| r.id == id) {
            (self.name, self.text) = (r.name.clone(), r.raw_text.clone());
            self.camera = self.original_camera.get(id).cloned()
                .unwrap_or_else(|| CameraSettings::parse(&self.text));
        } else { return }
        self.selected = Some(id.to_string());
        self.editing = false;
    }

    fn new_recipe(&mut self) {
        self.selected = None;
        self.name.clear();
        self.text.clear();
        self.camera = CameraSettings::default();
        self.editing = true;
        self.message = "Create a new Fujifilm camera recipe. It will be saved as a local draft, not written to the camera.".into();
    }

    fn new_from_as_shot_text(&mut self, text: &str) {
        self.new_recipe();
        self.name = "Fujifilm as-shot recipe".into();
        self.text = text.into();
        self.camera = CameraSettings::parse(&self.text);
        self.message = "Imported settings from the selected image's Fujifilm MakerNote into an unsaved local recipe. Check each value before saving or copying into a backup. The physical camera is unchanged.".into();
    }

    /// A PDF-sourced recipe is never edited in place. Until Save is used,
    /// changes live only in this editor; Save forks it into My Recipes.
    fn original_selected(&self) -> bool {
        self.selected.as_ref().is_some_and(|id| self.original.iter().any(|(_, recipe)| &recipe.id == id))
    }

    fn recipe_kind(&self) -> &'static str {
        if self.original_selected() { "ORIGINAL · READ-ONLY SOURCE" }
        else if self.selected.is_some() { "MY RECIPES · LOCAL DRAFT" }
        else { "NEW LOCAL RECIPE" }
    }

    fn save_label(&self) -> &'static str {
        if self.original_selected() { "Save copy to My Recipes" }
        else if self.selected.is_some() { "Save recipe changes" }
        else { "Create recipe in My Recipes" }
    }

    fn save(&mut self) {
        let (name, text) = (self.name.trim(), self.text.trim());
        if name.is_empty() || name.len() > 120 || text.len() > 16000 || (text.is_empty() && self.camera.values.is_empty()) {
            self.message = "Enter a recipe name (1–120 characters) and at least one camera setting; reference text may be left empty.".into();
            return;
        }
        let existing = self.selected.as_ref().and_then(|id| self.drafts.iter().position(|d| &d.id == id));
        // Original PDF records are immutable; saving one always forks a draft.
        let id = existing.and_then(|n| self.drafts.get(n).map(|d| d.id.clone())).unwrap_or_else(|| format!("custom-{}", next_draft_id(&self.drafts)));
        let local_slot = existing.and_then(|n| self.drafts.get(n)).and_then(|d| d.local_slot);
        let backup_label = existing.and_then(|n| self.drafts.get(n)).and_then(|d| d.backup_label.clone());
        let draft = RecipeDraft { id: id.clone(), name: name.to_string(), text: text.to_string(), camera: Some(self.camera.clone()), local_slot, backup_label };
        let mut updated = self.drafts.clone();
        if let Some(n) = existing { updated[n] = draft } else { updated.push(draft) }
        match persist_drafts(&updated) {
            Ok(()) => {
                self.drafts = updated;
                self.selected = Some(id);
                self.editing = false;
                self.message = "Saved as a local FujiX draft. Camera slots have not changed.".into();
            }
            Err(e) => self.message = format!("Could not save local recipe draft: {e}"),
        }
    }

    /// Create a complete offline C1–C7 bank, not additional physical camera
    /// memory. Each placeholder is immediately persisted so empty slots remain
    /// visible when the app relaunches, before the user edits any settings.
    fn create_named_backup(&mut self, name: Option<&str>) -> Result<u32, String> {
        let number = self.drafts.iter().filter_map(|d| d.local_slot.map(|s| s.backup))
            .max().unwrap_or(0).checked_add(1).ok_or("Local backup limit reached")?;
        let first_id = next_draft_id(&self.drafts);
        first_id.checked_add(6).ok_or("Local recipe ID limit reached")?;
        let label = name.map(str::trim).filter(|s| !s.is_empty())
            .unwrap_or("");
        if label.chars().count() > 80 { return Err("Backup name must be at most 80 characters".into()); }
        let mut updated = self.drafts.clone();
        let mut bank = local_backup_drafts(number, first_id);
        for slot in &mut bank { slot.backup_label = (!label.is_empty()).then(|| label.to_string()); }
        updated.extend(bank);
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.copy_target_backup = number;
        self.message = format!("{} created with seven editable local slots. Nothing was written to the camera.", self.backup_name(number));
        Ok(number)
    }

    fn backup_name(&self, number: u32) -> String {
        self.drafts.iter().filter(|d| d.local_slot.is_some_and(|s| s.backup == number))
            .find_map(|d| d.backup_label.as_ref().filter(|name| !name.trim().is_empty()).cloned())
            .unwrap_or_else(|| format!("Local Backup {number}"))
    }

    fn backup_ids(&self) -> Vec<u32> {
        self.local_backups().keys().copied().collect()
    }

    fn rename_local_backup(&mut self, number: u32, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 { return Err("Enter a backup name between 1 and 80 characters".into()); }
        let mut updated = self.drafts.clone();
        let mut changed = 0;
        for draft in &mut updated {
            if draft.local_slot.is_some_and(|slot| slot.backup == number) {
                draft.backup_label = Some(name.to_string());
                changed += 1;
            }
        }
        if changed != 7 { return Err("Backup is incomplete; refusing a partial rename".into()); }
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.message = format!("Backup renamed to {name}; all seven slot records were saved.");
        Ok(())
    }

    fn duplicate_local_backup(&mut self, number: u32) -> Result<u32, String> {
        let sources: Vec<RecipeDraft> = self.drafts.iter().filter(|d| d.local_slot.is_some_and(|s| s.backup == number)).cloned().collect();
        if sources.len() != 7 { return Err("Backup is incomplete; refusing to duplicate it".into()); }
        let new_number = self.backup_ids().last().copied().unwrap_or(0).checked_add(1).ok_or("Backup number limit reached")?;
        let first_id = next_draft_id(&self.drafts);
        first_id.checked_add(6).ok_or("Local recipe ID limit reached")?;
        let label = format!("{} (Copy)", self.backup_name(number));
        let label = label.chars().take(80).collect::<String>();
        let mut updated = self.drafts.clone();
        for slot in 1..=7u8 {
            let mut clone = sources.iter().find(|d| d.local_slot.is_some_and(|s| s.slot == slot))
                .ok_or("Backup has duplicate or missing slot records")?.clone();
            clone.id = format!("custom-{}", first_id + u64::from(slot - 1));
            clone.local_slot = Some(LocalBackupSlot { backup: new_number, slot });
            clone.backup_label = Some(label.clone());
            updated.push(clone);
        }
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.message = format!("Duplicated all seven slots into {label}. Original backup unchanged.");
        Ok(new_number)
    }

    fn delete_local_backup(&mut self, number: u32) -> Result<(), String> {
        let mut updated = self.drafts.clone();
        updated.retain(|d| !d.local_slot.is_some_and(|s| s.backup == number));
        if self.drafts.len() - updated.len() != 7 {
            return Err("Backup is incomplete; refusing an ambiguous deletion".into());
        }
        persist_drafts(&updated)?;
        if self.selected.as_ref().is_some_and(|id| !updated.iter().any(|d| &d.id == id) && !self.original.iter().any(|(_,r)| &r.id == id)) {
            self.selected = None;
            self.editing = false;
        }
        self.drafts = updated;
        self.message = format!("Deleted local backup {number} (all seven draft slots). Original backups unchanged.");
        Ok(())
    }

    /// Copy the unsaved editor values or selected original into an existing
    /// *local* C slot. The stable slot record ID, label and bank association
    /// never change; overwriting a populated slot needs a separate UI action.
    fn copy_current_into_slot(&mut self, number: u32, slot: u8) -> Result<(), String> {
        if !(1..=7).contains(&slot) { return Err("Target must be C1 through C7".into()); }
        let source_name = self.name.trim();
        if source_name.is_empty() || source_name.chars().count() > 120 || self.text.len() > 16000 || (self.camera.values.is_empty() && self.text.is_empty()) {
            return Err("Select or create a valid recipe with a name and camera settings first".into());
        }
        let mut updated = self.drafts.clone();
        let target = updated.iter_mut().find(|d| d.local_slot == Some(LocalBackupSlot { backup: number, slot }))
            .ok_or("The target backup or slot does not exist")?;
        target.name = source_name.into();
        target.text = self.text.clone();
        target.camera = Some(self.camera.clone());
        let selected = target.id.clone();
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.selected = Some(selected);
        self.editing = false;
        self.message = format!("Saved {source_name} into local {} C{slot}. No camera change.", self.backup_name(number));
        Ok(())
    }

    fn slot_has_content(&self, number: u32, slot: u8) -> bool {
        self.drafts.iter().find(|d| d.local_slot == Some(LocalBackupSlot { backup: number, slot }))
            .is_some_and(|d| !d.text.trim().is_empty() || d.camera.as_ref().is_some_and(|camera| !camera.values.is_empty()))
    }

    fn duplicate_selected_as_recipe(&mut self) -> Result<(), String> {
        if self.name.trim().is_empty() { return Err("Select a recipe to duplicate".into()); }
        let mut updated = self.drafts.clone();
        let id = format!("custom-{}", next_draft_id(&updated));
        let clone = RecipeDraft { id: id.clone(), name: format!("{} (Copy)", self.name.chars().take(110).collect::<String>()),
            text: self.text.clone(), camera: Some(self.camera.clone()), local_slot: None, backup_label: None };
        updated.push(clone);
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.select(&id);
        self.message = "Created a new independent editable recipe copy under My Other Recipes.".into();
        Ok(())
    }

    fn delete_or_clear_selected(&mut self) -> Result<(), String> {
        let id = self.selected.as_ref().ok_or("No local recipe selected")?.clone();
        let mut updated = self.drafts.clone();
        let pos = updated.iter().position(|d| d.id == id).ok_or("Original recipes cannot be deleted")?;
        let local_slot = updated[pos].local_slot;
        if let Some(s) = local_slot {
            updated[pos].name = format!("Untitled C{}", s.slot);
            updated[pos].text.clear();
            updated[pos].camera = Some(CameraSettings::default());
        } else { updated.remove(pos); }
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.selected = None;
        self.editing = false;
        self.message = if let Some(s) = local_slot {
            format!("Reset local backup {} C{} to an empty slot. Original camera untouched.", s.backup, s.slot)
        } else { "Deleted standalone local draft. Original source recipes unchanged.".into() };
        Ok(())
    }

    fn package_local_backup(&self, number: u32) -> Result<OfflineBackupPackage, String> {
        let mut slots: Vec<OfflineBackupSlot> = self.drafts.iter()
            .filter_map(|draft| draft.local_slot.filter(|s| s.backup == number)
                .map(|s| OfflineBackupSlot {
                    slot: s.slot,
                    name: draft.name.clone(),
                    text: draft.text.clone(),
                    camera: draft.camera.clone().unwrap_or_else(|| CameraSettings::parse(&draft.text)),
                }))
            .collect();
        slots.sort_by_key(|s| s.slot);
        let package = OfflineBackupPackage { kind: OFFLINE_BACKUP_KIND.into(), version: 1,
            name: self.backup_name(number), slots };
        package.validate()?;
        Ok(package)
    }

    fn export_local_backup(&mut self, number: u32) -> Result<std::path::PathBuf, String> {
        use std::io::Write;
        let package = self.package_local_backup(number)?;
        let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
        let directory = std::path::PathBuf::from(home).join("Documents/FujiX Chromaforge/Backups");
        std::fs::create_dir_all(&directory).map_err(|e| format!("Cannot create export folder: {e}"))?;
        let stem = package.name.chars()
            .filter_map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' { Some(ch) } else if ch.is_whitespace() { Some('-') } else { None })
            .take(48).collect::<String>();
        let stem = if stem.is_empty() { "Camera-Backup" } else { &stem };
        let bytes = serde_json::to_vec_pretty(&package).map_err(|e| e.to_string())?;
        for suffix in 0..10_000 {
            let file = directory.join(if suffix == 0 { format!("FujiX-{stem}-{number}.json") }
                else { format!("FujiX-{stem}-{number}-{suffix}.json") });
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&file) {
                Ok(mut handle) => {
                    if let Err(e) = handle.write_all(&bytes).and_then(|_| handle.sync_all()) {
                        let _ = std::fs::remove_file(&file);
                        return Err(format!("Export failed: {e}"));
                    }
                    self.message = format!("Exported seven local slots to {}. This is NOT a camera-compatible backup file.", file.display());
                    return Ok(file);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Export failed: {e}")),
            }
        }
        Err("Too many backup exports with this name".into())
    }

    fn import_local_backup(&mut self, input_path: &str) -> Result<u32, String> {
        let path = std::path::PathBuf::from(input_path.trim());
        if !path.is_absolute() || !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
            return Err("Paste an absolute path to a FujiX offline backup .json file".into());
        }
        let metadata = std::fs::metadata(&path).map_err(|e| format!("Backup file unavailable: {e}"))?;
        if !metadata.is_file() || metadata.len() > 1_048_576 {
            return Err("Backup import must be a regular JSON file smaller than 1 MiB".into());
        }
        let data = std::fs::read(&path).map_err(|e| format!("Cannot read backup: {e}"))?;
        let package: OfflineBackupPackage = serde_json::from_slice(&data).map_err(|e| format!("Invalid backup JSON: {e}"))?;
        package.validate()?;
        let number = self.backup_ids().last().copied().unwrap_or(0).checked_add(1).ok_or("Backup number limit reached")?;
        let first_id = next_draft_id(&self.drafts);
        first_id.checked_add(6).ok_or("Local recipe ID limit reached")?;
        let mut updated = self.drafts.clone();
        for slot in 1..=7u8 {
            let item = package.slots.iter().find(|item| item.slot == slot).ok_or("Backup missing a C-slot")?;
            updated.push(RecipeDraft {
                id: format!("custom-{}", first_id + u64::from(slot - 1)),
                name: item.name.clone(), text: item.text.clone(), camera: Some(item.camera.clone()),
                local_slot: Some(LocalBackupSlot { backup: number, slot }),
                backup_label: Some(package.name.clone()),
            });
        }
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.copy_target_backup = number;
        self.message = format!("Imported {} as a new seven-slot local backup. Original backups and camera unchanged.", package.name);
        Ok(number)
    }

    /// Import FilmKit's portable JSON preset as an independent local draft.
    /// Never interprets its numeric camera property IDs as permission to
    /// change an attached X100VI; this only persists our local recipe format.
    fn import_filmkit_file(&mut self, input_path: &str) -> Result<String, String> {
        let path = std::path::PathBuf::from(input_path.trim());
        if !path.is_absolute() || !path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("filmkit") || e.eq_ignore_ascii_case("json")) {
            return Err("Choose an absolute .filmkit or .json preset path".into());
        }
        let meta = std::fs::metadata(&path).map_err(|e| format!("FilmKit preset unavailable: {e}"))?;
        if !meta.is_file() || meta.len() > 1_048_576 { return Err("FilmKit file must be a regular file under 1 MiB".into()); }
        let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read FilmKit preset: {e}"))?;
        let fallback = path.file_stem().and_then(|s| s.to_str()).unwrap_or("FilmKit imported preset");
        let (name, camera) = filmkit::import(&bytes, fallback)?;
        let mut updated = self.drafts.clone();
        let id = format!("custom-{}", next_draft_id(&updated));
        updated.push(RecipeDraft {
            id: id.clone(), name, text: format!("Imported from FilmKit preset {}. Source file was not altered.", path.display()),
            camera: Some(camera), local_slot: None, backup_label: None,
        });
        persist_drafts(&updated)?;
        self.drafts = updated;
        self.select(&id);
        self.message = "FilmKit preset imported into My Other Recipes as an editable local draft. Review all parameters before copying to a local C-slot; camera unchanged.".into();
        Ok(id)
    }

    /// Export a FilmKit-compatible *portable* camera recipe. Unlike our
    /// seven-slot backup JSON, this is one editable look, never a camera
    /// settings backup or permission to write a physical C-slot.
    fn export_current_filmkit(&mut self) -> Result<std::path::PathBuf, String> {
        use std::io::Write;
        let bytes = filmkit::export(self.name.trim(), &self.camera)?;
        let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
        let directory = std::path::PathBuf::from(home).join("Documents/FujiX Chromaforge/FilmKit Presets");
        std::fs::create_dir_all(&directory).map_err(|e| format!("Cannot create FilmKit export folder: {e}"))?;
        let stem = self.name.chars()
            .filter_map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { Some(ch) }
                else if ch.is_whitespace() { Some('-') } else { None })
            .take(60).collect::<String>();
        let stem = if stem.is_empty() { "Fujifilm-Recipe" } else { &stem };
        for suffix in 0..10_000 {
            let path = directory.join(if suffix == 0 { format!("{stem}.filmkit") }
                else { format!("{stem}-{suffix}.filmkit") });
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
                        let _ = std::fs::remove_file(&path);
                        return Err(format!("FilmKit export failed: {error}"));
                    }
                    self.message = format!("Exported a FilmKit-compatible recipe to {}. Some FujiX reference-only fields are not understood by FilmKit. Camera unchanged.", path.display());
                    return Ok(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("FilmKit export failed: {error}")),
            }
        }
        Err("Too many FilmKit exports with this recipe name".into())
    }

    fn original_backups(&self) -> BTreeMap<u32, Vec<(u8, String, String)>> {
        let mut groups: BTreeMap<u32, Vec<(u8, String, String)>> = BTreeMap::new();
        for (backup, recipe) in &self.original {
            if let Some(slot) = recipe.id.rsplit_once("-slot-")
                .and_then(|(_, n)| n.parse::<u8>().ok()).filter(|n| (1..=7).contains(n)) {
                groups.entry(*backup).or_default().push((slot, recipe.id.clone(), recipe.name.clone()));
            }
        }
        for slots in groups.values_mut() { slots.sort_by_key(|(slot, _, _)| *slot); }
        groups
    }

    fn local_backups(&self) -> BTreeMap<u32, Vec<(u8, String, String)>> {
        let mut groups: BTreeMap<u32, Vec<(u8, String, String)>> = BTreeMap::new();
        for d in &self.drafts {
            if let Some(s) = d.local_slot.filter(|s| (1..=7).contains(&s.slot)) {
                groups.entry(s.backup).or_default().push((s.slot, d.id.clone(), d.name.clone()));
            }
        }
        for slots in groups.values_mut() { slots.sort_by_key(|(slot, _, _)| *slot); }
        groups
    }

    fn camera_for(&self, id: &str) -> Option<CameraSettings> {
        if let Some(draft) = self.drafts.iter().find(|d| d.id == id) {
            return Some(draft.camera.clone().unwrap_or_else(|| CameraSettings::parse(&draft.text)));
        }
        self.original_camera.get(id).cloned()
    }
}

fn local_backup_drafts(backup: u32, first_id: u64) -> Vec<RecipeDraft> {
    (1..=7u8).map(|slot| RecipeDraft {
        id: format!("custom-{}", first_id + u64::from(slot - 1)),
        name: format!("Untitled C{slot}"), text: String::new(),
        camera: Some(CameraSettings::default()),
        local_slot: Some(LocalBackupSlot { backup, slot }),
        backup_label: None,
    }).collect()
}

const STATE_ID: &str = "fuji-x100vi-native-panel";
const FOLDER_NAME: &str = "Fuji X100VI Recipes";

/// Additional camera-recipe library *inside Browse Profiles*, not a new
/// Develop profile, LUT or photo-processing preset. The original backups have
/// camera C1–C7 slot folders, in their source order; missing slots are shown
/// explicitly. Custom banks are separate offline seven-slot collections.
pub(super) fn show_in_profiles(app: &mut LccApp, ui: &mut egui::Ui, photo: PhotoId, base: &DevelopSettings) {
    let ctx = ui.ctx().clone();
    let top = ui.cursor().top();
    let key = egui::Id::new(STATE_ID);
    let mut state: FujiState = ctx.data(|d| d.get_temp(key)).unwrap_or_else(FujiState::load);
    let originals = state.original_backups();
    let locals = state.local_backups();
    let standalone: Vec<(String, String)> = state.drafts.iter().filter(|d| d.local_slot.is_none())
        .map(|d| (d.id.clone(), d.name.clone())).collect();
    let mut action: Option<ProfileAction> = None;

    if profile_group(ui, "root", "Fujifilm", &format!("{}", state.original.len() + state.drafts.len()), 0, true).open {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 14, top: 4, bottom: 8 }).show(ui, |ui| {
            ui.label(RichText::new("HOVER TO PREVIEW · CLICK TO EDIT · APPROXIMATE LOOKS")
                .size(10.0).color(Tokens::get(ui.ctx()).text_dim));
            ui.horizontal(|ui| {
                let expand = ui.small_button("Expand all backups");
                register(ui.ctx(), "fujixProfiles:expandAll", expand.rect);
                if expand.clicked() {
                    set_all_backup_groups(ui.ctx(), originals.keys().copied(), locals.keys().copied(), true);
                }
                let collapse = ui.small_button("Collapse all");
                register(ui.ctx(), "fujixProfiles:collapseAll", collapse.rect);
                if collapse.clicked() {
                    set_all_backup_groups(ui.ctx(), originals.keys().copied(), locals.keys().copied(), false);
                }
            });
        });
        if profile_group(ui, "film-simulations", "Film Simulations", &FILM_SIMULATIONS.len().to_string(), 1, false).open {
            let looks: Vec<_> = FILM_SIMULATIONS.iter().map(|simulation| {
                let mut camera = CameraSettings::default();
                camera.set("film_simulation", *simulation);
                ProfileTile { id: Some(format!("simulation:{simulation}")), name: (*simulation).to_owned(), slot: None, camera: Some(camera) }
            }).collect();
            if let Some(id) = profile_tile_grid(app, ui, photo, base, &looks, "fujixProfileSimulation") {
                action = Some(ProfileAction::NewFromSimulation(id.trim_start_matches("simulation:").to_owned()));
            }
        }
        if profile_group(ui, "my-backups", "My Camera Backups", &format!("{}", locals.len()), 1, true).open {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 25, right: 12, top: 5, bottom: 9 }).show(ui, |ui| {
                ui.label(RichText::new("Create an unlimited local 7-slot backup set").size(10.0).color(Tokens::get(ui.ctx()).text_dim));
                let name = ui.add(egui::TextEdit::singleline(&mut state.new_backup_name)
                    .hint_text("New backup name (optional)").desired_width(f32::INFINITY));
                register(ui.ctx(), "fujixBackup:newName", name.rect);
                let create = ui.small_button("+ New 7-slot camera backup");
                register(ui.ctx(), "fujixBackup:create", create.rect);
                if create.clicked() {
                    action = Some(ProfileAction::CreateBackup(state.new_backup_name.clone()));
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Import a previously exported FujiX offline backup JSON:")
                    .size(10.0).color(Tokens::get(ui.ctx()).text_dim));
                let import = ui.add(egui::TextEdit::singleline(&mut state.import_backup_path)
                    .hint_text("Paste full file path from Finder").desired_width(f32::INFINITY));
                register(ui.ctx(), "fujixBackup:importPath", import.rect);
                let import_button = ui.add_enabled(!state.import_backup_path.trim().is_empty(),
                    egui::Button::new("Import backup JSON"));
                register(ui.ctx(), "fujixBackup:import", import_button.rect);
                if import_button.clicked() {
                    action = Some(ProfileAction::ImportBackup(state.import_backup_path.clone()));
                }
            });
            for (number, slots) in &locals {
                let name = state.backup_name(*number);
                if profile_group(ui, &format!("my-backup-{number}"),
                    &name, "7 slots", 2, false).open {
                    egui::Frame::NONE.inner_margin(egui::Margin { left: 29, right: 12, top: 3, bottom: 7 }).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            let rename = ui.small_button("Rename");
                            register(ui.ctx(), format!("fujixBackup:{number}:rename"), rename.rect);
                            if rename.clicked() {
                                state.rename_backup = Some(*number);
                                state.rename_buffer = name.clone();
                                state.confirm_delete_backup = None;
                            }
                            let duplicate = ui.small_button("Duplicate");
                            register(ui.ctx(), format!("fujixBackup:{number}:duplicate"), duplicate.rect);
                            if duplicate.clicked() { action = Some(ProfileAction::DuplicateBackup(*number)); }
                            let export = ui.small_button("Export JSON");
                            register(ui.ctx(), format!("fujixBackup:{number}:export"), export.rect);
                            if export.clicked() { action = Some(ProfileAction::ExportBackup(*number)); }
                            let delete = ui.small_button("Delete");
                            register(ui.ctx(), format!("fujixBackup:{number}:delete"), delete.rect);
                            if delete.clicked() { state.confirm_delete_backup = Some(*number); }
                        });
                        if state.rename_backup == Some(*number) {
                            let edit = ui.add(egui::TextEdit::singleline(&mut state.rename_buffer)
                                .hint_text("Name (1–80 characters)").desired_width(f32::INFINITY));
                            register(ui.ctx(), format!("fujixBackup:{number}:renameValue"), edit.rect);
                            ui.horizontal(|ui| {
                                let save = ui.small_button("Save name");
                                register(ui.ctx(), format!("fujixBackup:{number}:saveName"), save.rect);
                                if save.clicked() {
                                    action = Some(ProfileAction::RenameBackup(*number, state.rename_buffer.clone()));
                                }
                                if ui.small_button("Cancel").clicked() { state.rename_backup = None; }
                            });
                        }
                        if state.confirm_delete_backup == Some(*number) {
                            ui.label(RichText::new("Delete all seven LOCAL slots in this backup?")
                                .color(Color32::from_rgb(224, 160, 107)).size(10.5));
                            let confirm = ui.small_button("Confirm delete backup");
                            register(ui.ctx(), format!("fujixBackup:{number}:confirmDelete"), confirm.rect);
                            if confirm.clicked() { action = Some(ProfileAction::DeleteBackup(*number)); }
                            if ui.small_button("Cancel").clicked() { state.confirm_delete_backup = None; }
                        }
                    });
                    let tiles = backup_tiles(&state, slots);
                    if let Some(id) = profile_tile_grid(app, ui, photo, base, &tiles,
                        &format!("fujixProfileGroup:my-backup-{number}-slot")) {
                        action = Some(ProfileAction::Select(id));
                    }
                }
            }
        }
        if profile_group(ui, "my-recipes", "My Other Recipes", &standalone.len().to_string(), 1, false).open {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 25, right: 12, top: 4, bottom: 7 }).show(ui, |ui| {
                if ui.small_button("+ New camera recipe").clicked() {
                    action = Some(ProfileAction::NewRecipe);
                }
                ui.label(RichText::new("Import FilmKit .filmkit / .json recipe as a local draft:")
                    .size(10.0).color(Tokens::get(ui.ctx()).text_dim));
                let path = ui.add(egui::TextEdit::singleline(&mut state.import_filmkit_path)
                    .hint_text("Full FilmKit preset file path").desired_width(f32::INFINITY));
                register(ui.ctx(), "fujixFilmkit:importPath", path.rect);
                let button = ui.add_enabled(!state.import_filmkit_path.trim().is_empty(),
                    egui::Button::new("Import FilmKit preset"));
                register(ui.ctx(), "fujixFilmkit:import", button.rect);
                if button.clicked() {
                    action = Some(ProfileAction::ImportFilmKit(state.import_filmkit_path.clone()));
                }
            });
            let tiles: Vec<_> = standalone.iter().map(|(id, name)| ProfileTile {
                id: Some(id.clone()), name: name.clone(), slot: None, camera: state.camera_for(id),
            }).collect();
            if let Some(id) = profile_tile_grid(app, ui, photo, base, &tiles, "fujixProfileRecipe") {
                action = Some(ProfileAction::Select(id));
            }
        }
        if profile_group(ui, "original-backups", "Original Camera Backups", &originals.len().to_string(), 1, false).open {
            for (backup, slots) in &originals {
                if profile_group(ui, &format!("source-backup-{backup}"),
                    &format!("Backup {backup}"), &format!("{}/7", slots.len()), 2, false).open {
                    let tiles = backup_tiles(&state, slots);
                    if let Some(id) = profile_tile_grid(app, ui, photo, base, &tiles,
                        &format!("fujixProfileGroup:source-backup-{backup}-slot")) {
                        action = Some(ProfileAction::Select(id));
                    }
                }
            }
        }
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 14, top: 7, bottom: 12 }).show(ui, |ui| {
            ui.label(RichText::new("Hover previews use approximate desktop rendering, not exact X100VI processing. Click a recipe to edit. Camera and photo originals remain unchanged.")
                .size(10.0).color(Tokens::get(ui.ctx()).text_dim));
            if !state.message.is_empty() {
                ui.label(RichText::new(&state.message).size(10.5).color(Tokens::get(ui.ctx()).text_dim));
            }
        });
    }

    match action {
        Some(ProfileAction::Select(id)) => {
            state.select(&id);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("fujix-edit-camera-settings-open"), true));
            app.ui.right = RightPanel::Edit;
        }
        Some(ProfileAction::NewRecipe) => {
            state.new_recipe();
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("fujix-edit-camera-settings-open"), true));
            app.ui.right = RightPanel::Edit;
        }
        Some(ProfileAction::NewFromSimulation(simulation)) => {
            state.new_recipe();
            state.name = format!("{simulation} recipe");
            state.camera.set("film_simulation", simulation);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("fujix-edit-camera-settings-open"), true));
            app.ui.right = RightPanel::Edit;
        }
        Some(ProfileAction::CreateBackup(name)) => match state.create_named_backup(Some(&name)) {
            Ok(num) => {
                state.new_backup_name.clear();
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(("fujix-profile-folder", format!("my-backup-{num}"))), true));
            }
            Err(error) => state.message = format!("Unable to create local camera backup: {error}"),
        },
        Some(ProfileAction::RenameBackup(num, name)) => match state.rename_local_backup(num, &name) {
            Ok(()) => state.rename_backup = None,
            Err(error) => state.message = format!("Rename failed: {error}"),
        },
        Some(ProfileAction::DuplicateBackup(num)) => match state.duplicate_local_backup(num) {
            Ok(new_num) => { ctx.data_mut(|d| d.insert_temp(egui::Id::new(("fujix-profile-folder", format!("my-backup-{new_num}"))), true)); }
            Err(error) => state.message = format!("Duplicate failed: {error}"),
        },
        Some(ProfileAction::ExportBackup(num)) => {
            if let Err(error) = state.export_local_backup(num) { state.message = format!("Export failed: {error}"); }
        }
        Some(ProfileAction::ImportBackup(path)) => match state.import_local_backup(&path) {
            Ok(new_num) => {
                state.import_backup_path.clear();
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(("fujix-profile-folder", format!("my-backup-{new_num}"))), true));
            }
            Err(error) => state.message = format!("Import failed: {error}"),
        },
        Some(ProfileAction::DeleteBackup(num)) => match state.delete_local_backup(num) {
            Ok(()) => state.confirm_delete_backup = None,
            Err(error) => state.message = format!("Delete failed: {error}"),
        },
        Some(ProfileAction::ImportFilmKit(path)) => match state.import_filmkit_file(&path) {
            Ok(_) => {
                state.import_filmkit_path.clear();
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("fujix-edit-camera-settings-open"), true));
                app.ui.right = RightPanel::Edit;
            }
            Err(error) => state.message = format!("FilmKit import failed: {error}"),
        },
        None => {}
    }
    // The ordinary profile browser can restore an old (non-Fujifilm) hover
    // later in this frame. Clear only that stale stored profile; preserve the
    // transient Fujifilm look selected by this frame's hovered recipe row.
    let bottom = ui.cursor().top();
    if ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| {
        p.y >= top && p.y < bottom && p.x >= ui.min_rect().left() && p.x <= ui.max_rect().right()
    }) {
        ctx.data_mut(|d| d.remove::<(lcc_catalog::PhotoId, String)>(egui::Id::new("last-profile-hover")));
    }
    ctx.data_mut(|d| d.insert_temp(key, state));
}

enum ProfileAction {
    Select(String), NewRecipe, NewFromSimulation(String), CreateBackup(String),
    RenameBackup(u32, String), DuplicateBackup(u32), ExportBackup(u32),
    ImportBackup(String), DeleteBackup(u32), ImportFilmKit(String),
}

struct ProfileGroup { open: bool }

/// An offline camera slot, standalone saved draft, or base simulation look.
/// The source backup remains immutable; unavailable C-slots are still shown.
struct ProfileTile {
    id: Option<String>,
    name: String,
    slot: Option<u8>,
    camera: Option<CameraSettings>,
}

/// Match Chromaforge's Basic profile browser: 2 large cards per row, 4:3
/// live previews and the profile name beneath, with a C-slot badge on camera
/// banks. A missing source slot has a visibly empty card, never a fake look.
fn backup_tiles(state: &FujiState, records: &[(u8, String, String)]) -> Vec<ProfileTile> {
    (1..=7).map(|slot| {
        let entry = records.iter().find(|(s, _, _)| *s == slot);
        ProfileTile {
            id: entry.map(|(_, id, _)| id.clone()),
            name: entry.map(|(_, _, name)| name.clone()).unwrap_or_else(|| "Empty slot".into()),
            camera: entry.and_then(|(_, id, _)| state.camera_for(id)),
            slot: Some(slot),
        }
    }).collect()
}

fn set_all_backup_groups(
    ctx: &egui::Context,
    originals: impl Iterator<Item = u32>,
    locals: impl Iterator<Item = u32>,
    open: bool,
) {
    ctx.data_mut(|d| {
        for id in ["original-backups", "my-backups"] {
            d.insert_temp(egui::Id::new(("fujix-profile-folder", id)), open);
        }
        for number in originals {
            d.insert_temp(egui::Id::new(("fujix-profile-folder", format!("source-backup-{number}"))), open);
        }
        for number in locals {
            d.insert_temp(egui::Id::new(("fujix-profile-folder", format!("my-backup-{number}"))), open);
        }
    });
}

const FUJI_PROFILE_COLUMNS: usize = 2;
const FUJI_THUMB_EDGE: usize = 256;

fn profile_tile_grid(
    app: &mut LccApp, ui: &mut egui::Ui, photo: PhotoId,
    base: &DevelopSettings, tiles: &[ProfileTile], prefix: &str,
) -> Option<String> {
    let mut clicked = None;
    let gap = 8.0;
    let width = ui.available_width();
    // Identical horizontal geometry to Basic, Film and Cinematic profiles.
    let cell_width = ((width - 44.0 - gap) / FUJI_PROFILE_COLUMNS as f32).max(40.0);
    let image_height = (cell_width * 0.75).round();
    let height = image_height + 39.0;
    for row in tiles.chunks(FUJI_PROFILE_COLUMNS) {
        let (rr, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let visible = ui.is_rect_visible(rr.expand(image_height));
        for (i, tile) in row.iter().enumerate() {
            let cell = Rect::from_min_size(
                pos2(rr.left() + 22.0 + i as f32 * (cell_width + gap), rr.top()),
                vec2(cell_width, height),
            );
            let identity = match (tile.slot, tile.id.as_deref()) {
                (Some(slot), _) => format!("{prefix}-{slot}"),
                (_, Some(id)) => format!("{prefix}:{id}"),
                _ => continue,
            };
            let resp = ui.interact(cell, egui::Id::new(&identity), Sense::click());
            register(ui.ctx(), identity, cell);
            crate::widgets::announce(&resp, match tile.slot {
                Some(slot) => format!("Fujifilm C{slot} · {} · approximate hover preview", tile.name),
                None => format!("Fujifilm {} · approximate hover preview", tile.name),
            });
            let t = Tokens::get(ui.ctx());
            let image = Rect::from_min_size(cell.min, vec2(cell_width, image_height));
            ui.painter().rect_filled(image, 3.0, t.canvas);
            let has_look = (visible || resp.hovered())
                && render_recipe_preview(app, ui, photo, base, &tile.name, tile.camera.as_ref(), image, resp.hovered());
            if resp.hovered() {
                ui.painter().rect_stroke(image, 3.0,
                    egui::Stroke::new(1.5, t.text_label), egui::StrokeKind::Outside);
            }
            if let Some(slot) = tile.slot {
                let corner = Rect::from_min_size(image.min + vec2(6.0, 6.0), vec2(28.0, 20.0));
                ui.painter().rect_filled(corner, 3.0, Color32::from_black_alpha(180));
                ui.painter().text(corner.center(), Align2::CENTER_CENTER,
                    format!("C{slot}"), t.semibold(11.0), Color32::WHITE);
            }
            if visible && !has_look {
                let missing = if tile.id.is_none() { "EMPTY" } else { "NO PREVIEW" };
                ui.painter().text(image.center(), Align2::CENTER_CENTER,
                    missing, t.font(10.0), t.text_dim);
            }
            let name = crate::widgets::one_line(ui.painter(), &tile.name,
                t.font(12.0), t.text_label, cell_width - 2.0);
            ui.painter().galley(
                pos2(cell.left() + 2.0, image.bottom() + 13.0 - name.size().y / 2.0),
                name, t.text_label,
            );
            if resp.clicked() && let Some(id) = &tile.id {
                clicked = Some(id.clone());
            }
            resp.on_hover_text(match tile.slot {
                Some(slot) => format!("C{slot}: {} · approximate preview on hover · click to edit", tile.name),
                None => format!("{} · approximate preview on hover · click to edit", tile.name),
            });
        }
    }
    ui.add_space(6.0);
    clicked
}

fn profile_group(ui: &mut egui::Ui, id: &str, label: &str, count: &str, depth: usize, default_open: bool) -> ProfileGroup {
    let key = egui::Id::new(("fujix-profile-folder", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(default_open);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    register(ui.ctx(), format!("fujixProfileGroup:{id}"), rect);
    crate::widgets::announce(&resp, label.to_owned());
    let t = Tokens::get(ui.ctx());
    let start = rect.left() + 7.0 + depth as f32 * 11.0;
    if resp.hovered() { ui.painter().rect_filled(rect, 0.0, t.hover.gamma_multiply(0.6)); }
    paint(ui.painter(), Rect::from_center_size(pos2(start + 10.0, rect.center().y), vec2(14.0, 14.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_label);
    let text_x = start + 22.0;
    let max_label = (rect.right() - 43.0 - text_x).max(30.0);
    let font = t.semibold(if depth <= 1 { 12.5 } else { 11.5 });
    let galley = crate::widgets::one_line(ui.painter(), label, font, t.text_label, max_label);
    ui.painter().galley(pos2(text_x, rect.center().y - galley.size().y / 2.0), galley, t.text_label);
    ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER,
        count, t.font(10.0), t.text_dim);
    if resp.clicked() { open = !open; ui.data_mut(|d| d.insert_temp(key, open)); }
    ProfileGroup { open }
}

fn render_recipe_preview(app: &mut LccApp, ui: &mut egui::Ui, photo: PhotoId,
    base: &DevelopSettings, name: &str, camera: Option<&CameraSettings>, thumb: Rect, hovered: bool) -> bool {
    let settings = camera.and_then(|camera| preview::approximate(base, camera));
    let has_preview = settings.is_some();
    if hovered {
        app.hover_preview = Some(crate::HoverPreview {
            label: if has_preview {
                format!("Fujifilm approximate preview: {name}")
            } else {
                format!("Fujifilm {name}: no usable recipe settings; original photo shown")
            },
            settings: settings.clone().unwrap_or_else(|| base.clone()),
        });
    }
    // A blank custom slot or incomplete source is not given fabricated camera
    // properties. Hovering it restores the original photo with an explanation.
    let Some(settings) = settings else { return false; };
    // Only ask the shared renderer for visible cells. Expanded backups can
    // contain 101 source recipes, so do not queue offscreen photo variants.
    if ui.is_rect_visible(thumb) {
        let t = Tokens::get(ui.ctx());
        ui.painter().rect_filled(thumb, 2.0, t.canvas);
        if let Some(job) = app.session.variant_job(photo, &settings, FUJI_THUMB_EDGE)
            && let Some(image) = app.renderer.variant(job) {
            ui.painter().image(image.id(), thumb, super::presets::cover_uv(thumb, image.size), Color32::WHITE);
        }
    }
    true
}

/// The Develop toolbar's Presets button toggles the *existing* Chromaforge
/// Classic left panel. Keeping photo adjustments on the right lets a recipe
/// selected from the folder open its camera-only controls in Develop.
pub(super) fn toggle_native_presets(app: &mut LccApp) {
    let shown = app.ui.module == ModuleId::Develop && app.ui.left_panel
        && !app.ui.presets && !app.ui.hidden_panels.contains(&PanelId::Presets)
        && crate::panels::classic::is_open_with_key(app, crate::panels::presets::IN_COLUMN_KEY);
    if shown {
        crate::panels::classic::set_open_with_key(app, PanelId::Presets,
            crate::panels::presets::IN_COLUMN_KEY, false);
        return;
    }
    open_native_presets(app);
}

/// Open, rather than toggle, the existing Classic presets column. This is
/// also used by the Fuji section's empty state: "Browse recipes" must never
/// accidentally close an already-open presets browser.
fn open_native_presets(app: &mut LccApp) {
    app.ui.module = ModuleId::Develop;
    app.ui.left_panel = true;
    app.ui.presets = false; // never replace the Edit panel with the old right-column Presets view
    app.ui.hidden_panels.retain(|panel| *panel != PanelId::Presets);
    crate::panels::classic::set_open_with_key(app, PanelId::Presets,
        crate::panels::presets::IN_COLUMN_KEY, true);
}

/// A special-purpose *camera-recipe* folder in Chromaforge's genuine Presets
/// browser. Camera settings are not secretly converted to Chromaforge image
/// processing presets: their camera-specific values cannot be reproduced
/// faithfully by the desktop RAW engine. Clicking a row opens the native
/// recipe editor, where the original text is accessible and can be forked.
/// The original 101 records and user-created drafts use the exact same state
/// as the adjacent FujiX Film Recipes and Camera C1–C7 toolbar modes.
pub(super) fn show_in_preset_folders(app: &mut LccApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let key = egui::Id::new(STATE_ID);
    let mut s: FujiState = ctx.data(|d| d.get_temp(key)).unwrap_or_else(FujiState::load);
    let header_open = folder_header(ui, "fujix-root", FOLDER_NAME,
        &format!("{}", s.original.len() + s.drafts.len()), 0.0, true);
    let mut action: Option<FolderAction> = None;

    if header_open {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 22, right: 12, top: 3, bottom: 6 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 28.0).max(60.0);
                let response = ui.add_sized(vec2(w, 25.0), egui::TextEdit::singleline(&mut s.search)
                    .hint_text("Search X100VI recipes…"));
                register(ui.ctx(), "fujixPresetSearch", response.rect);
                if ui.add_enabled(!s.search.is_empty(), egui::Button::new("×").frame(false))
                    .on_hover_text("Clear recipe search").clicked() {
                    s.search.clear();
                }
            });
        });
        let custom_rows: Vec<(String, String)> = s.drafts.iter()
            .filter(|r| contains(&s.search, &r.name, &r.text))
            .map(|r| (r.id.clone(), r.name.clone())).collect();
        let original_rows: Vec<(String, String)> = s.original.iter()
            .filter(|(_, r)| contains(&s.search, &r.name, &r.raw_text))
            .map(|(_, r)| (r.id.clone(), r.name.clone())).collect();
        // Put editable drafts and the camera manager above the 101-entry
        // original catalog. Expanding Originals must not push My Recipes or
        // the C1–C7 action thousands of points below the viewport.
        let my_open = folder_header(ui, "fujix-custom", "My Recipes",
            &format!("{}", custom_rows.len()), 1.0, true);
        if my_open {
            draw_rows(ui, &custom_rows, &s, &mut action);
            if ui.add_sized(vec2(ui.available_width(), 26.0),
                egui::Button::new(RichText::new("+ New X100VI recipe").size(11.5))).clicked() {
                action = Some(FolderAction::New);
            }
        }
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 27.0), Sense::click());
        register(ui.ctx(), "fujixPresetCameraSlots", r);
        let t = Tokens::get(ui.ctx());
        ui.painter().text(pos2(r.left() + 39.0, r.center().y), Align2::LEFT_CENTER,
            "Manage camera slots C1–C7", t.font(11.5), t.text_dim);
        if resp.clicked() { action = Some(FolderAction::Camera); }
        if resp.hovered() { ui.painter().rect_stroke(r, 0.0,
            egui::Stroke::new(1.0, t.divider), egui::StrokeKind::Inside); }
        let orig_open = folder_header(ui, "fujix-original", "Original Recipes",
            &format!("{}", original_rows.len()), 1.0, !s.search.is_empty());
        if orig_open {
            draw_rows(ui, &original_rows, &s, &mut action);
        }
        if !s.search.is_empty() && custom_rows.is_empty() && original_rows.is_empty() {
            ui.label(RichText::new("No matching X100VI recipes").color(t.text_dim).size(11.0));
        }
    }

    match action {
        Some(FolderAction::Select(id)) => {
            s.select(&id);
            // Selecting a preset should reveal its controls immediately, even
            // when this section was folded for a previous photograph.
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("fujix-edit-camera-settings-open"), true));
            app.ui.presets = false;
            // A recipe click opens the *normal Edit workspace*, where the
            // FujiX camera controls are added above the photo Light/Color/
            // Effects sections. On an empty library use the standalone
            // native recipe editor, which does not require a photo.
            app.ui.right = if app.session.active().is_some() { RightPanel::Edit } else { RightPanel::FujiRecipes };
            app.ui.module = crate::module::ModuleId::Develop;
            app.ui.view = crate::state::ViewMode::Detail;
        }
        Some(FolderAction::New) => {
            s.new_recipe();
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("fujix-edit-camera-settings-open"), true));
            app.ui.presets = false;
            app.ui.right = if app.session.active().is_some() { RightPanel::Edit } else { RightPanel::FujiRecipes };
            app.ui.module = crate::module::ModuleId::Develop;
            app.ui.view = crate::state::ViewMode::Detail;
        }
        Some(FolderAction::Camera) => {
            app.ui.right = RightPanel::FujiCamera;
            app.ui.view = crate::state::ViewMode::Detail;
        }
        None => {}
    }
    ctx.data_mut(|d| d.insert_temp(key, s));
}

enum FolderAction { Select(String), New, Camera }

fn folder_header(ui: &mut egui::Ui, id: &str, label: &str, count: &str, indent: f32, initial: bool) -> bool {
    let key = egui::Id::new(("fujix-preset-folder", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(initial);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    register(ui.ctx(), format!("presetGroup:{label}"), r);
    let t = Tokens::get(ui.ctx());
    let left = r.left() + indent * 12.0;
    paint(ui.painter(), Rect::from_center_size(pos2(left + 22.0, r.center().y), vec2(16.0, 16.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_label);
    if resp.hovered() { ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.6)); }
    ui.painter().text(pos2(left + 36.0, r.center().y), Align2::LEFT_CENTER, label, t.semibold(12.5), t.text_label);
    ui.painter().text(pos2(r.right() - 9.0, r.center().y), Align2::RIGHT_CENTER, count, t.font(10.0), t.text_dim);
    if resp.clicked() { open = !open; ui.data_mut(|d| d.insert_temp(key, open)); }
    open
}

fn draw_rows(ui: &mut egui::Ui, rows: &[(String, String)], state: &FujiState, action: &mut Option<FolderAction>) {
    let t = Tokens::get(ui.ctx());
    const H: f32 = 26.0;
    // Same virtualization strategy as native LightCraft preset groups. 101
    // camera recipes must not cost 101 GUI response allocations per frame.
    let view = ui.clip_rect().expand(H);
    let top = ui.cursor().top();
    let first = (((view.top() - top) / H).floor() as usize).min(rows.len());
    let end = (((view.bottom() - top) / H).ceil() as usize).clamp(first, rows.len());
    if first > 0 { ui.allocate_space(vec2(ui.available_width(), first as f32 * H)); }
    for (id, name) in rows.get(first..end).unwrap_or_default() {
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), H), Sense::click());
        register(ui.ctx(), format!("fujixPreset:{id}"), r);
        if state.selected.as_deref() == Some(id) {
            ui.painter().rect_filled(r, 0.0, t.tool_active);
        } else if resp.hovered() { ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.7)); }
        let label = if name.chars().count() > 37 { format!("{}…", name.chars().take(36).collect::<String>()) } else { name.clone() };
        ui.painter().text(pos2(r.left() + 52.0, r.center().y), Align2::LEFT_CENTER,
            label, t.font(12.0), t.text_label);
        let clicked = resp.clicked();
        resp.on_hover_text("Open native FujiX recipe settings; not an automatic Lightroom-style look");
        if clicked {
            *action = Some(FolderAction::Select(id.clone()));
        }
    }
    if end < rows.len() { ui.allocate_space(vec2(ui.available_width(), (rows.len() - end) as f32 * H)); }
}

fn next_draft_id(drafts: &[RecipeDraft]) -> u64 {
    drafts.iter().filter_map(|r| r.id.strip_prefix("custom-").and_then(|s| s.parse::<u64>().ok())).max().unwrap_or(0).saturating_add(1)
}

fn draft_path() -> Option<std::path::PathBuf> {
    #[cfg(target_arch = "wasm32")]
    { None }
    #[cfg(not(target_arch = "wasm32"))]
    { std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("Library/Application Support/FujiX Chromaforge/recipe-drafts.json")) }
}

fn legacy_draft_path() -> Option<std::path::PathBuf> {
    #[cfg(target_arch = "wasm32")]
    { None }
    #[cfg(not(target_arch = "wasm32"))]
    { std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("Library/Application Support/FujiX LightCraft/recipe-drafts.json")).filter(|path| path.exists()) }
}

fn persist_drafts(drafts: &[RecipeDraft]) -> Result<(), String> {
    let path = draft_path().ok_or("Local recipe saving requires a native desktop build")?;
    let parent = path.parent().ok_or("Invalid local recipe path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let encoded = serde_json::to_vec_pretty(drafts).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.part");
    std::fs::write(&temp, encoded).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let id = egui::Id::new(STATE_ID);
    let mut s: FujiState = ctx.data(|data| data.get_temp(id)).unwrap_or_else(FujiState::load);
    if app.ui.right == RightPanel::FujiCamera { show_camera(ui, &mut s); }
    else if show_recipes(ui, &mut s) {
        let _ = app.run("panel.fujiCamera", serde_json::json!({}));
    }
    ctx.data_mut(|data| data.insert_temp(id, s));
}

/// Native X100VI controls inserted *within the existing Chromaforge Edit mode*.
/// The ordinary Chromaforge photo sliders below still adjust photo pixels;
/// these controls edit only the selected camera-recipe draft. No camera writes.
pub(super) fn show_in_edit(app: &mut LccApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let id = egui::Id::new(STATE_ID);
    let mut s: FujiState = ctx.data(|d| d.get_temp(id)).unwrap_or_else(FujiState::load);
    let open_id = egui::Id::new("fujix-edit-camera-settings-open");
    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id))
        .unwrap_or(s.selected.is_some() || s.editing);
    let (response, _) = section_header(ui, "fujixCamera", "Fujifilm X100VI · Camera Recipe", open, None);
    if response.clicked() { open = !open; ctx.data_mut(|d| d.insert_temp(open_id, open)); }
    if open {
        let embedded_camera_recipe_available = app.session.active()
            .and_then(|photo| app.session.catalog.photo(photo))
            .is_some_and(|photo| matches!(photo.source, lcc_catalog::Source::File { .. }));
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 5 }).show(ui, |ui| {
            let button = ui.add_enabled(embedded_camera_recipe_available,
                egui::Button::new("New recipe from selected Fuji image's camera settings"));
            register(ui.ctx(), "fujixRecipe:fromImageMetadata", button.rect);
            if button.clicked() {
                match app.run("photo.fujiRecipe", serde_json::json!({})) {
                    Ok(data) if data["present"] == true => {
                        if let Some(text) = data["text"].as_str().filter(|s| s.len() <= 16_000 && !s.trim().is_empty()) {
                            s.new_from_as_shot_text(text);
                        } else {
                            s.message = "The Fujifilm metadata was empty or too large; no recipe was created.".into();
                        }
                    }
                    Ok(_) => s.message = "This image does not contain a readable Fujifilm MakerNote recipe. No changes were made.".into(),
                    Err(error) => s.message = format!("Could not inspect selected image's Fujifilm settings: {error}"),
                }
            }
            if !embedded_camera_recipe_available {
                ui.label(RichText::new("Select an imported RAF or Fujifilm JPEG to read its MakerNote settings, or create a recipe manually.")
                    .size(10.0).color(Tokens::get(ui.ctx()).text_dim));
            }
        });
        if s.selected.is_none() && !s.editing {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 10, bottom: 12 }).show(ui, |ui| {
                let t = Tokens::get(ui.ctx());
                ui.label(RichText::new("Camera recipes").font(t.semibold(12.5)).color(t.text));
                ui.label(RichText::new(format!("{} original X100VI recipes · {} local drafts", s.original.len(), s.drafts.len()))
                    .size(11.0).color(t.text_dim));
                ui.add_space(6.0);
                if ui.button("Browse recipes in Presets").clicked() {
                    open_native_presets(app);
                }
                if ui.small_button("+ New camera recipe").clicked() {
                    s.new_recipe();
                }
                ui.add_space(5.0);
                ui.label(RichText::new("Camera recipes are separate from the photo's Develop adjustments.")
                    .size(10.5).color(t.text_dim));
            });
        } else {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 10 }).show(ui, |ui| {
            let t = Tokens::get(ui.ctx());
            ui.label(RichText::new(s.recipe_kind()).size(10.0).color(t.text_dim));
            ui.add_space(5.0);
            ui.label(RichText::new("Recipe name").size(11.0).color(t.text_dim));
            if ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(f32::INFINITY))
                .changed() { changed(&mut s); }
            ui.label(RichText::new("Camera recipe settings · independent of photo adjustments")
                .size(11.0).color(t.text_dim));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Presets").on_hover_text("Show Fuji X100VI Recipes in the native Presets column").clicked() {
                    open_native_presets(app);
                }
                if ui.button(s.save_label()).clicked() { s.save(); }
                if ui.add_enabled(s.editing && s.selected.is_some(), egui::Button::new("Discard changes")).clicked() {
                    if let Some(id) = s.selected.clone() { s.select(&id); }
                }
            });
        });
        show_recipe_library_actions(ui, &mut s);
        recipe_controls(ui, &mut s);
        camera_section(ui, "fujixSourceInDevelop", "Source Notes & Original Reference", false, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 5, bottom: 10 }).show(ui, |ui| {
                let t = Tokens::get(ui.ctx());
                ui.label(RichText::new("Preserved source text. Editing these notes does not automatically change the camera settings above.")
                    .size(10.5).color(t.text_dim));
                if ui.add(egui::TextEdit::multiline(&mut s.text).desired_rows(7)
                    .desired_width(f32::INFINITY).font(egui::TextStyle::Monospace)).changed() {
                    changed(&mut s);
                }
            });
        });
        if !s.message.is_empty() {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 9 }).show(ui, |ui| {
                ui.label(RichText::new(&s.message).color(Tokens::get(ui.ctx()).text_dim).size(10.5));
            });
        }
        }
    }
    divider(ui);
    ctx.data_mut(|d| d.insert_temp(id, s));
}

/// Local library operations are distinct from physical X100VI operations.
/// Everything below persists only into the FujiX Chromaforge draft JSON.
fn show_recipe_library_actions(ui: &mut egui::Ui, state: &mut FujiState) {
    if state.selected.is_none() && !state.editing { return; }
    let backups = state.backup_ids();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 13 }).show(ui, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.label(RichText::new("LOCAL RECIPE LIBRARY ACTIONS").size(10.0).color(t.text_dim));
        let duplicate = ui.small_button("Duplicate as new recipe");
        register(ui.ctx(), "fujixRecipe:duplicate", duplicate.rect);
        if duplicate.clicked() {
            if let Err(error) = state.duplicate_selected_as_recipe() {
                state.message = format!("Duplicate failed: {error}");
            }
        }
        let export = ui.small_button("Export as FilmKit preset");
        register(ui.ctx(), "fujixRecipe:exportFilmkit", export.rect);
        if export.clicked() {
            if let Err(error) = state.export_current_filmkit() {
                state.message = format!("FilmKit export failed: {error}");
            }
        }
        if let Some(id) = state.selected.clone()
            && state.drafts.iter().any(|draft| draft.id == id) {
            let label = if state.drafts.iter().any(|d| d.id == id && d.local_slot.is_some()) {
                "Reset this local C-slot" } else { "Delete this local draft" };
            let delete = ui.small_button(label);
            register(ui.ctx(), "fujixRecipe:delete", delete.rect);
            if delete.clicked() { state.confirm_delete_recipe = Some(id.clone()); }
            if state.confirm_delete_recipe.as_ref() == Some(&id) {
                ui.label(RichText::new("Confirm deletion? This affects local files only.")
                    .color(Color32::from_rgb(224, 160, 107)).size(10.5));
                let confirm = ui.small_button("Confirm reset/delete");
                register(ui.ctx(), "fujixRecipe:confirmDelete", confirm.rect);
                if confirm.clicked() {
                    match state.delete_or_clear_selected() {
                        Ok(()) => state.confirm_delete_recipe = None,
                        Err(error) => state.message = format!("Delete failed: {error}"),
                    }
                }
                if ui.small_button("Cancel").clicked() { state.confirm_delete_recipe = None; }
            }
        }
        if backups.is_empty() {
            ui.label(RichText::new("Create a local camera backup in Browse Profiles to save this recipe into C1–C7.")
                .size(10.0).color(t.text_dim));
            return;
        }
        ui.add_space(5.0);
        ui.label(RichText::new("Copy this recipe into an offline backup C-slot:")
            .size(10.5).color(t.text_dim));
        if !backups.contains(&state.copy_target_backup) {
            state.copy_target_backup = backups[0];
        }
        if !(1..=7).contains(&state.copy_target_slot) { state.copy_target_slot = 1; }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("fujix-copy-backup")
                .selected_text(state.backup_name(state.copy_target_backup))
                .width(ui.available_width().min(240.0))
                .show_ui(ui, |ui| {
                    for &number in &backups {
                        let label = state.backup_name(number);
                        ui.selectable_value(&mut state.copy_target_backup, number, label);
                    }
                });
            egui::ComboBox::from_id_salt("fujix-copy-slot")
                .selected_text(format!("C{}", state.copy_target_slot)).show_ui(ui, |ui| {
                    for slot in 1..=7u8 {
                        ui.selectable_value(&mut state.copy_target_slot, slot, format!("C{slot}"));
                    }
                });
        });
        let target = LocalBackupSlot { backup: state.copy_target_backup, slot: state.copy_target_slot };
        let save = ui.small_button(format!("Copy into {} C{}", state.backup_name(target.backup), target.slot));
        register(ui.ctx(), "fujixRecipe:copyToSlot", save.rect);
        if save.clicked() {
            if state.slot_has_content(target.backup, target.slot) {
                state.pending_slot_overwrite = Some(target);
                state.message = format!("C{} already has a saved recipe. Confirm replacement before changing any local files.", target.slot);
            } else if let Err(error) = state.copy_current_into_slot(target.backup, target.slot) {
                state.message = format!("Copy failed: {error}");
            }
        }
        if state.pending_slot_overwrite == Some(target) {
            ui.label(RichText::new(format!("Replace saved {} C{}? Existing local slot contents will be overwritten.",
                state.backup_name(target.backup), target.slot))
                .size(10.5).color(Color32::from_rgb(224, 160, 107)));
            let replace = ui.small_button("Confirm replace local slot");
            register(ui.ctx(), "fujixRecipe:confirmReplace", replace.rect);
            if replace.clicked() {
                match state.copy_current_into_slot(target.backup, target.slot) {
                    Ok(()) => state.pending_slot_overwrite = None,
                    Err(error) => state.message = format!("Copy failed: {error}"),
                }
            }
            if ui.small_button("Cancel replacement").clicked() { state.pending_slot_overwrite = None; }
        }
        ui.label(RichText::new("Local offline backups may exceed seven sets. Only seven physical X100VI C-slots exist; this never uploads to the camera.")
            .size(10.0).color(t.text_dim));
    });
}

fn camera_section(ui: &mut egui::Ui, id: &str, title: &str, initially_open: bool,
    body: impl FnOnce(&mut egui::Ui)) {
    let state_id = egui::Id::new(("fujix-camera-section", id));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(state_id)).unwrap_or(initially_open);
    let (response, _) = section_header(ui, id, title, open, None);
    if response.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(state_id, open));
    }
    if open { body(ui); }
    divider(ui);
}

fn changed(s: &mut FujiState) {
    s.editing = true;
    s.message = "Unsaved recipe changes · Camera C1–C7 slots unchanged.".into();
}

fn camera_choice(ui: &mut egui::Ui, s: &mut FujiState, key: &str, label: &str, choices: &[&str]) {
    let t = Tokens::get(ui.ctx());
    let selected = s.camera.get(key).to_owned();
    let name = if selected.is_empty() { "Not specified" } else { selected.as_str() };
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 5, bottom: 8 }).show(ui, |ui| {
        ui.label(RichText::new(label).size(11.5).color(t.text_dim));
        let r = crate::widgets::dropdown_within(ui, &format!("fujix.{key}"), name,
            t.font(13.0), t.text_label, ui.available_width().min(255.0));
        let mut next: Option<String> = None;
        egui::Popup::menu(&r).show(|ui| {
            if ui.selectable_label(selected.is_empty(), "Not specified").clicked() {
                next = Some(String::new());
            }
            for &value in choices {
                if ui.selectable_label(selected.eq_ignore_ascii_case(value), value).clicked() {
                    next = Some(value.to_owned());
                }
            }
        });
        if let Some(value) = next {
            s.camera.set(key, value);
            changed(s);
        }
    });
}

fn camera_number(ui: &mut egui::Ui, s: &mut FujiState, id: &'static str, label: &'static str,
    lo: f64, hi: f64, step: f64, section: Section) {
    let value = s.camera.number(id);
    if let Some(v) = value {
        let spec = ControlSpec { id, label, section, min: lo, max: hi, default: 0.0,
            step, decimals: if step < 1.0 { 1 } else { 0 }, track: Track::Centered, shade: lcc_develop::Shade::Plain };
        let result = slider(ui, &spec, v, true, None);
        if let Some(n) = result.value {
            s.camera.set_number(id, n.clamp(lo, hi));
            changed(s);
        }
    } else {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 7, bottom: 7 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{label}: Not specified")).color(Tokens::get(ui.ctx()).text_dim).size(12.0));
                if ui.small_button("Set").clicked() {
                    s.camera.set_number(id, 0.0);
                    changed(s);
                }
            });
        });
    }
}

fn camera_text_field(ui: &mut egui::Ui, s: &mut FujiState, key: &str, label: &str, hint: &str) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.label(RichText::new(label).color(Tokens::get(ui.ctx()).text_dim).size(11.5));
        let mut value = s.camera.get(key).to_string();
        let res = ui.add(egui::TextEdit::singleline(&mut value).hint_text(hint).desired_width(f32::INFINITY));
        register(ui.ctx(), format!("fujixField:{key}"), res.rect);
        if res.changed() {
            s.camera.set(key, value);
            changed(s);
        }
    });
}

fn recipe_controls(ui: &mut egui::Ui, s: &mut FujiState) {
    camera_section(ui, "fujixSimulation", "Film Simulation & Dynamic Range", true, |ui| {
        camera_choice(ui, s, "film_simulation", "Film Simulation", FILM_SIMULATIONS);
        camera_choice(ui, s, "dynamic_range", "Dynamic Range", &["Auto", "DR100", "DR200", "DR400"]);
        camera_choice(ui, s, "d_range_priority", "D-Range Priority", &["Off", "Auto", "Weak", "Strong"]);
    });
    camera_section(ui, "fujixLight", "Light · Highlights & Shadows", true, |ui| {
        camera_number(ui, s, "highlight", "Highlight Tone", -2.0, 4.0, 0.5, Section::Light);
        camera_number(ui, s, "shadow", "Shadow Tone", -2.0, 4.0, 0.5, Section::Light);
        camera_text_field(ui, s, "exposure_compensation", "Exposure Compensation · reference only", "+1/3 to +2/3");
    });
    camera_section(ui, "fujixColor", "Color & White Balance", false, |ui| {
        camera_number(ui, s, "color", "Color", -4.0, 4.0, 1.0, Section::Color);
        camera_choice(ui, s, "white_balance", "White Balance Mode", &[
            "As Shot", "Auto", "Auto White Priority", "Auto Ambience Priority", "Daylight", "Shade",
            "Fluorescent 1", "Fluorescent 2", "Fluorescent 3", "Incandescent", "Underwater",
            "Kelvin",
        ]);
        // A Kelvin temperature has its own field. Two widgets writing the
        // same white_balance key made the menu and free-text input fight.
        if s.camera.get("white_balance") == "Kelvin" || !s.camera.get("white_balance_kelvin").is_empty() {
            camera_text_field(ui, s, "white_balance_kelvin", "Color Temperature", "e.g. 6300K");
        }
        camera_number(ui, s, "wb_red", "WB Shift · Red", -9.0, 9.0, 1.0, Section::Color);
        camera_number(ui, s, "wb_blue", "WB Shift · Blue", -9.0, 9.0, 1.0, Section::Color);
    });
    camera_section(ui, "fujixGrain", "Grain & Color Chrome Effects", false, |ui| {
        camera_choice(ui, s, "grain_strength", "Grain Effect · Strength", &["Off", "Weak", "Strong"]);
        camera_choice(ui, s, "grain_size", "Grain Effect · Size", &["Small", "Large"]);
        camera_choice(ui, s, "color_chrome", "Color Chrome Effect", &["Off", "Weak", "Strong"]);
        camera_choice(ui, s, "color_chrome_blue", "Color Chrome FX Blue", &["Off", "Weak", "Strong"]);
        camera_choice(ui, s, "smooth_skin", "Smooth Skin Effect", &["Off", "Weak", "Strong"]);
    });
    camera_section(ui, "fujixDetail", "Sharpness · Noise Reduction · Clarity", false, |ui| {
        camera_number(ui, s, "sharpness", "Sharpness", -4.0, 4.0, 1.0, Section::Detail);
        camera_number(ui, s, "noise_reduction", "High ISO Noise Reduction", -4.0, 4.0, 1.0, Section::Detail);
        camera_number(ui, s, "clarity", "Clarity", -5.0, 5.0, 1.0, Section::Detail);
        camera_text_field(ui, s, "iso", "ISO · reference only", "Auto up to ISO 6400");
    });
    camera_section(ui, "fujixMonochrome", "Monochrome Warm/Cool & Magenta/Green", false, |ui| {
        camera_number(ui, s, "mono_warm_cool", "Monochromatic Warm/Cool", -9.0, 9.0, 1.0, Section::Color);
        camera_number(ui, s, "mono_magenta_green", "Monochromatic Magenta/Green", -9.0, 9.0, 1.0, Section::Color);
    });
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 9, bottom: 12 }).show(ui, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.label(RichText::new("These settings are saved as camera-recipe drafts, not Chromaforge photo adjustments. Fujifilm's in-camera simulations and grain cannot be reproduced identically by the desktop renderer.")
            .size(10.5).color(t.text_dim));
    });
}

fn header(ui: &mut egui::Ui, text: &str, description: &str) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 18, right: 16, top: 15, bottom: 12 }).show(ui, |ui| {
        ui.label(RichText::new(text).font(t.semibold(16.0)).color(t.text));
        ui.label(RichText::new(description).font(t.font(11.0)).color(t.text_dim));
    });
    crate::widgets::divider(ui);
}

fn show_recipes(ui: &mut egui::Ui, s: &mut FujiState) -> bool {
    let t = Tokens::get(ui.ctx());
    header(ui, "FujiX · Film Recipes", "X100VI · original recipes and local drafts");
    let mut open_camera = false;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 17, right: 14, top: 9, bottom: 12 }).show(ui, |ui| {
        ui.label(RichText::new(format!("{} original recipes · {} drafts", s.original.len(), s.drafts.len())).color(t.text_dim).size(11.0));
        ui.horizontal(|ui| {
            if ui.button("+ New recipe").clicked() { s.new_recipe(); }
            if ui.button("Camera C1–C7").clicked() { open_camera = true; }
        });
        ui.add_space(8.0);
        ui.add(egui::TextEdit::singleline(&mut s.search).hint_text("Search 101 recipes…").desired_width(f32::INFINITY));
        let matches: Vec<(String, String, String)> = s.drafts.iter()
            .filter(|r| contains(&s.search, &r.name, &r.text))
            .map(|r| (r.id.clone(), r.name.clone(), "MY RECIPES · LOCAL DRAFT".into()))
            .chain(s.original.iter().filter(|(_,r)| contains(&s.search, &r.name, &r.raw_text))
                .map(|(b,r)| (r.id.clone(), r.name.clone(), format!("ORIGINAL · Backup {b} · page {}", r.page))))
            .collect();
        ui.label(RichText::new(format!("{} matching", matches.len())).size(10.0).color(t.text_dim));
        egui::ScrollArea::vertical().id_salt("fujix-recipe-list").max_height(124.0).show(ui, |ui| {
            for (id, name, meta) in matches {
                let selected = s.selected.as_deref() == Some(&id);
                let resp = ui.selectable_label(selected, RichText::new(name).size(12.0));
                crate::widgets::register(ui.ctx(), format!("fujixRecipe:{id}"), resp.rect);
                if resp.clicked() { s.select(&id); }
                if selected { ui.label(RichText::new(meta).size(10.0).color(t.text_dim)); }
            }
        });
        divider(ui);
        if s.selected.is_some() || s.editing {
            ui.add_space(8.0);
            ui.label(RichText::new(s.recipe_kind()).size(10.0).color(t.text_dim));
            ui.label(RichText::new("Recipe name").size(11.0).color(t.text_dim));
            if ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(f32::INFINITY))
                .changed() { changed(s); }
            ui.horizontal_wrapped(|ui| {
                if ui.button(s.save_label()).clicked() { s.save(); }
                if ui.add_enabled(s.editing && s.selected.is_some(), egui::Button::new("Discard changes")).clicked() {
                    if let Some(id) = s.selected.clone() { s.select(&id); }
                }
            });
        } else {
            ui.label(RichText::new("Select a recipe to inspect or edit, or create a new one.").size(11.0).color(t.text_dim));
        }
        show_recipe_library_actions(ui, s);
        if !s.message.is_empty() { ui.add_space(6.0);ui.label(RichText::new(&s.message).size(11.0).color(t.text)); }
    });
    if s.selected.is_some() || s.editing {
        recipe_controls(ui, s);
        camera_section(ui, "fujixSource", "Original Text & Reference", false, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 10 }).show(ui, |ui| {
                ui.label(RichText::new("Original reference is retained alongside editable camera settings. Corrections here do not silently overwrite the parsed controls.").size(10.5).color(t.text_dim));
                if ui.add(egui::TextEdit::multiline(&mut s.text).desired_rows(10)
                    .desired_width(f32::INFINITY).font(egui::TextStyle::Monospace)).changed() {
                    changed(s);
                }
            });
        });
    }
    open_camera
}

fn contains(search: &str, name: &str, text: &str) -> bool {
    let term = search.trim().to_lowercase();
    term.is_empty() || name.to_lowercase().contains(&term) || text.to_lowercase().contains(&term)
}

fn show_camera(ui: &mut egui::Ui, s: &mut FujiState) {
    let t = Tokens::get(ui.ctx());
    header(ui, "X100VI · Camera Recipes", "Native camera connection and custom-slot management");
    egui::Frame::NONE.inner_margin(egui::Margin { left: 18, right: 16, top: 14, bottom: 14 }).show(ui, |ui| {
        ui.label(RichText::new("Camera connection").color(t.text).size(13.0).strong());
        ui.label(RichText::new("Connect the X100VI by USB-C, and select USB RAW CONV./BACKUP RESTORE on the camera. Close X RAW STUDIO and other software holding USB.").color(t.text_dim).size(11.0));
        ui.add_space(8.0);
        ui.label(RichText::new("No persistent camera session · native camera probe available below").color(Color32::from_rgb(210, 169, 113)).size(11.0));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (busy, detail, verified, raw_mode_detail, raw_mode_verified) = s.probe.lock().ok()
                .map(|p| (p.busy, p.detail.clone(), p.verified, p.raw_mode_detail.clone(), p.raw_mode_verified))
                .unwrap_or((false, "Could not read probe state".into(), false, String::new(), false));
            if ui.add_enabled(!busy, egui::Button::new(if busy { "Reading X100VI USB identity…" } else { "Verify X100VI USB connection" })).clicked() {
                start_probe(Arc::clone(&s.probe), ui.ctx().clone(), false);
            }
            let slot_reader_installed = std::env::current_exe().ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("fujix-x100vi-current-slot")))
                .is_some_and(|reader| reader.is_file());
            if ui.add_enabled(!busy && slot_reader_installed,
                egui::Button::new("Read CURRENT camera C-slot (read-only)"))
                .on_disabled_hover_text("The validated read-only current-slot helper has not been bundled with this app; the control stays disabled.")
                .clicked() {
                start_probe(Arc::clone(&s.probe), ui.ctx().clone(), true);
            }
            if !detail.is_empty() {
                ui.label(RichText::new(detail).size(11.0).color(if verified { Color32::from_rgb(105, 202, 158) } else { t.text_dim }));
            }
            if let Some(summary) = s.probe.lock().ok().and_then(|p| p.active_slot.clone()) {
                ui.label(RichText::new(summary).size(11.0).color(t.text));
            }
            if verified {
                ui.label(RichText::new("Read-only probe completed and closed its PTP session. A single current-slot read is NOT a seven-slot backup. Full slot scan and write are still locked.").size(10.0).color(t.text_dim));
            }
            let full_reader_installed = std::env::current_exe().ok()
                .and_then(|exe| exe.parent().map(|folder| folder.join("fujix-x100vi-current-slot-full-readonly")))
                .is_some_and(|bin| bin.is_file());
            let full_read = ui.add_enabled(!busy && full_reader_installed,
                egui::Button::new("Capture all 26 properties of CURRENT C-slot (read-only)"));
            register(ui.ctx(), "fujixBridge:fullCurrentSlot", full_read.rect);
            if full_read.clicked() { start_full_current_slot_probe(Arc::clone(&s.probe), ui.ctx().clone()); }
            if let Some((message, ok)) = s.probe.lock().ok()
                .map(|probe| (probe.full_current_detail.clone(), probe.full_current_verified))
                .filter(|(message, _)| !message.is_empty()) {
                ui.label(RichText::new(message).size(10.5)
                    .color(if ok { Color32::from_rgb(105, 202, 158) } else { t.text_dim }));
            }
            ui.label(RichText::new("A complete current-slot capture is NOT all C1–C7. Physical slot switching is never performed here. Select each slot manually on the camera and capture twice for a future full-bank audit.")
                .size(10.0).color(t.text_dim));
            ui.add_space(8.0);
            ui.label(RichText::new("Camera-assisted RAW processing").color(t.text).size(12.0).strong());
            ui.label(RichText::new("The offline renderer works without USB. The optional X100VI bridge checks USB RAW CONV. mode and PTP compatibility; no file upload or camera setting write is performed.")
                .size(10.5).color(t.text_dim));
            ui.add_space(6.0);
            ui.label(RichText::new("Validate source RAF before any camera transfer (offline)").size(10.5).color(t.text_dim));
            let path = ui.add(egui::TextEdit::singleline(&mut s.raf_preflight_path)
                .hint_text("Absolute path to the original X100VI .RAF")
                .desired_width(f32::INFINITY));
            register(ui.ctx(), "fujixPreflight:rafPath", path.rect);
            let preflight_button = ui.add_enabled(!busy && !s.raf_preflight_path.trim().is_empty(),
                egui::Button::new("Verify RAF hash and save source audit"));
            register(ui.ctx(), "fujixPreflight:verifyRAF", preflight_button.rect);
            if preflight_button.clicked() {
                start_raf_preflight(Arc::clone(&s.probe), ui.ctx().clone(), s.raf_preflight_path.clone());
            }
            if let Some((message, ok)) = s.probe.lock().ok()
                .map(|p| (p.raf_preflight_detail.clone(), p.raf_preflight_verified))
                .filter(|(m, _)| !m.is_empty()) {
                ui.label(RichText::new(message).size(10.5)
                    .color(if ok { Color32::from_rgb(105, 202, 158) } else { t.text_dim }));
            }
            ui.label(RichText::new("Preflight only: preserves the RAW file and saves a local audit JSON; it does not upload anything or prove that camera conversion works.")
                .size(10.0).color(t.text_dim));
            let bridge_installed = std::env::current_exe().ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("fujix-camera-bridge")))
                .is_some_and(|path| path.is_file());
            let button = ui.add_enabled(!busy && bridge_installed,
                egui::Button::new("Check X100VI RAW-conversion mode (read-only)"));
            register(ui.ctx(), "fujixBridge:capabilities", button.rect);
            if button.clicked() {
                start_raw_mode_probe(Arc::clone(&s.probe), ui.ctx().clone());
            }
            if !raw_mode_detail.is_empty() {
                ui.label(RichText::new(raw_mode_detail).size(10.5)
                    .color(if raw_mode_verified { Color32::from_rgb(105, 202, 158) } else { t.text_dim }));
            }
            ui.add_enabled(false, egui::Button::new("Use X100VI camera-accurate RAF previews"))
                .on_disabled_hover_text("Camera conversion transport and JPEG readback are not yet validated. The native bridge only verifies USB identity and mode.");
            ui.label(RichText::new("Camera conversion cannot be enabled until a real RAF upload → camera render → verified JPEG retrieval succeeds. Offline preview remains available.")
                .size(10.0).color(t.text_dim));
        }
        #[cfg(target_arch = "wasm32")]
        ui.label(RichText::new("Native USB probe requires the macOS app build.").size(11.0).color(t.text_dim));
        ui.add_space(12.0);
        ui.label(RichText::new("Camera slot").color(t.text_dim).size(11.0));
        egui::ComboBox::from_id_salt("fujix-camera-slot").selected_text(format!("C{}", s.camera_slot)).show_ui(ui, |ui| {
            for slot in 1..=7 { ui.selectable_value(&mut s.camera_slot, slot, format!("C{slot}")); }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Recipe for selected slot").color(t.text_dim).size(11.0));
        let candidates: Vec<(String, String)> = s.original.iter().map(|(_, r)| (r.id.clone(), r.name.clone()))
            .chain(s.drafts.iter().map(|r| (r.id.clone(), format!("{} · local draft", r.name)))).collect();
        let mut choice = s.selected.clone();
        let choice_name = candidates.iter().find(|(id,_)| Some(id) == choice.as_ref())
            .map(|(_,name)| name.as_str()).unwrap_or("Choose a FujiX recipe…");
        egui::ComboBox::from_id_salt("fujix-camera-recipe")
            .width(ui.available_width().min(255.0))
            .selected_text(choice_name)
            .show_ui(ui, |ui| {
                for (id,name) in &candidates { ui.selectable_value(&mut choice, Some(id.clone()), name); }
            });
        if choice != s.selected {
            if let Some(id) = choice.as_deref() { s.select(id); }
        }
        if s.selected.is_some() {
            ui.label(RichText::new("Staged only: no value has been sent to the camera.").color(t.text_dim).size(10.0));
            let staged = s.camera.as_camera_text(&s.name);
            egui::ScrollArea::vertical().id_salt("fujix-stage-settings").max_height(120.0).show(ui, |ui| {
                ui.label(RichText::new(&staged).monospace().size(10.0).color(t.text_dim));
            });
        }
        ui.add_space(8.0);
        ui.add_enabled(false, egui::Button::new("Read and back up all C1–C7 slots")).on_disabled_hover_text("Safe native USB/PTP transfer must be validated on a physical X100VI before this is enabled.");
        ui.add_enabled(false, egui::Button::new(format!("Write recipe into C{}", s.camera_slot))).on_disabled_hover_text("Locked until seven-slot backup, selected-slot confirmation, and on-camera readback are verified.");
        crate::widgets::divider(ui);
        ui.label(RichText::new("Camera safety").color(t.text).size(12.0).strong());
        ui.label(RichText::new("Writing camera properties can overwrite existing recipes. A complete original backup, restoreable slot selection and verified write/readback are required before enabling any write. The original 101 FujiX recipes are available from the adjacent Recipes tool.").color(t.text_dim).size(11.0));
        ui.add_space(8.0);
        ui.label(RichText::new("The camera panel and USB probe run natively in Chromaforge, without a browser or separate editor.").color(t.text_dim).size(10.0));
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn start_probe(shared: Arc<Mutex<UsbProbe>>, ctx: egui::Context, read_active_slot: bool) {
    if let Ok(mut p) = shared.lock() {
        if p.busy { return; }
        p.busy = true;
        p.verified = false;
        p.active_slot = None;
        p.detail = "Opening read-only macOS ImageCaptureCore PTP session…".into();
    } else { return; }
    let report = Arc::clone(&shared);
    let repaint = ctx.clone();
    let task = std::thread::Builder::new().name("fujix-camera-readonly".into()).spawn(move || {
        let binary_name = if read_active_slot { "fujix-x100vi-current-slot" } else { "fujix-x100vi-probe" };
        let (verified, detail, current) = match std::env::current_exe().ok()
            .and_then(|exe| exe.parent().map(|folder| folder.join(binary_name))) {
            Some(binary) if binary.is_file() => match std::process::Command::new(binary).output() {
                Ok(result) => {
                    if read_active_slot { classify_current_slot(&result.stdout, result.status.success()) }
                    else { let (ok, detail) = classify_probe(&result.stdout, &result.stderr, result.status.success()); (ok, detail, None) }
                }
                Err(e) => (false, format!("Unable to open native camera reader: {e}"), None),
            },
            _ => (false, "FujiX read-only camera helper is missing from the app bundle; install the latest custom build.".into(), None),
        };
        if let Ok(mut p) = report.lock() {
            p.busy = false;
            p.detail = detail;
            p.verified = verified;
            p.active_slot = current;
        }
        repaint.request_repaint();
    });
    if let Err(e) = task {
        if let Ok(mut p) = shared.lock() {
            p.busy = false;
            p.detail = format!("Camera identity worker did not start: {e}");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_raw_mode_probe(shared: Arc<Mutex<UsbProbe>>, ctx: egui::Context) {
    if let Ok(mut probe) = shared.lock() {
        if probe.busy { return; }
        probe.busy = true;
        probe.raw_mode_verified = false;
        probe.raw_mode_detail = "Checking X100VI USB mode through a read-only PTP capability transaction…".into();
    } else { return; }
    let report = Arc::clone(&shared);
    let task = std::thread::Builder::new().name("fujix-raw-mode-readonly".into()).spawn(move || {
        let detail = std::env::current_exe().ok()
            .and_then(|exe| exe.parent().map(|folder| folder.join("fujix-camera-bridge")))
            .filter(|bin| bin.is_file())
            .map(|bin| std::process::Command::new(bin).arg("--capabilities").output());
        let (verified, msg) = match detail {
            Some(Ok(output)) => classify_raw_mode(&output.stdout, output.status.success()),
            Some(Err(error)) => (false, format!("Unable to check camera RAW mode: {error}")),
            None => (false, "Native X100VI RAW capability helper not installed.".into()),
        };
        if let Ok(mut probe) = report.lock() {
            probe.busy = false;
            probe.raw_mode_verified = verified;
            probe.raw_mode_detail = msg;
        }
        ctx.request_repaint();
    });
    if let Err(error) = task {
        if let Ok(mut probe) = shared.lock() {
            probe.busy = false;
            probe.raw_mode_detail = format!("Camera capability worker unavailable: {error}");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_raf_preflight(shared: Arc<Mutex<UsbProbe>>, ctx: egui::Context, original_path: String) {
    if let Ok(mut probe) = shared.lock() {
        if probe.busy { return; }
        probe.busy = true;
        probe.raf_preflight_verified = false;
        probe.raf_preflight_detail = "Checking original RAF header and SHA-256 without changing its contents…".into();
    } else { return; }
    let report = Arc::clone(&shared);
    let task = std::thread::Builder::new().name("fujix-original-raf-preflight".into()).spawn(move || {
        let result = preflight::inspect_and_save(&original_path);
        if let Ok(mut probe) = report.lock() {
            probe.busy = false;
            match result {
                Ok((evidence, filename)) => {
                    probe.raf_preflight_verified = true;
                    probe.raf_preflight_detail = format!("Offline RAF preflight passed: X100VI · {} bytes · SHA-256 {}. Audit: {}. USB transfer, JPEG readback, seven-slot camera backup and restore are NOT yet verified.",
                        evidence.raf_size, evidence.raf_sha256, filename.display());
                }
                Err(error) => {
                    probe.raf_preflight_verified = false;
                    probe.raf_preflight_detail = format!("RAF preflight refused: {error}. Original file was not modified.");
                }
            }
        }
        ctx.request_repaint();
    });
    if let Err(error) = task {
        if let Ok(mut probe) = shared.lock() {
            probe.busy = false;
            probe.raf_preflight_detail = format!("Cannot start RAF integrity check: {error}");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_full_current_slot_probe(shared: Arc<Mutex<UsbProbe>>, ctx: egui::Context) {
    if let Ok(mut probe) = shared.lock() {
        if probe.busy { return; }
        probe.busy = true;
        probe.full_current_verified = false;
        probe.full_current_detail = "Reading only the currently active physical C-slot, with 26 raw PTP fields and device serial…".into();
    } else { return; }
    let report = Arc::clone(&shared);
    let task = std::thread::Builder::new().name("fujix-full-current-slot-readonly".into()).spawn(move || {
        let outcome = std::env::current_exe().ok()
            .and_then(|exe| exe.parent().map(|folder| folder.join("fujix-x100vi-current-slot-full-readonly")))
            .filter(|bin| bin.is_file())
            .map(|bin| std::process::Command::new(bin).output());
        let (verified, detail) = match outcome {
            Some(Ok(result)) => validate_and_save_full_current_slot(&result.stdout, result.status.success()),
            Some(Err(error)) => (false, format!("Could not read full current C-slot: {error}")),
            None => (false, "Complete current-slot helper is not bundled with FujiX Chromaforge.".into()),
        };
        if let Ok(mut probe) = report.lock() {
            probe.busy = false;
            probe.full_current_verified = verified;
            probe.full_current_detail = detail;
        }
        ctx.request_repaint();
    });
    if let Err(error) = task {
        if let Ok(mut probe) = shared.lock() {
            probe.busy = false;
            probe.full_current_detail = format!("Cannot start current-slot PTP reader: {error}");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_and_save_full_current_slot(stdout: &[u8], exited_ok: bool) -> (bool, String) {
    use std::io::Write;
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return (false, "Current-slot reader did not return valid JSON; nothing was saved.".into());
    };
    if !exited_ok || json["ok"] != true || json["model"] != "X100VI"
        || json["onlyCurrentSlot"] != true || json["writeCommands"] != false
        || json["complete26Properties"] != true {
        let error = json["error"].as_str().unwrap_or("incomplete or unverified read-only capture");
        return (false, format!("Current-slot full capture refused: {}. No full-bank backup was claimed.",
            error.chars().take(160).collect::<String>()));
    }
    let Some(slot) = json["activeSlot"].as_u64().filter(|n| (1..=7).contains(n)) else {
        return (false, "Current C-slot selector was missing or invalid.".into());
    };
    let Some(serial) = json["serial"].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 80) else {
        return (false, "No authenticated PTP serial in this capture.".into());
    };
    let Some(properties) = json["properties"].as_object().filter(|p| p.len() == 26) else {
        return (false, "Current-slot capture is incomplete: expected 26 property records.".into());
    };
    if properties.get("D18C").and_then(|p| p["value"].as_u64()) != Some(slot) {
        return (false, "D18C readback does not identify the reported active C-slot.".into());
    }
    if json["slotName"].as_str().is_none_or(|name| name.is_empty() || name.len() > 200) {
        return (false, "Current slot has no verified PTP preset name.".into());
    }
    for code in 0xD18Cu16..=0xD1A5u16 {
        let key = format!("{code:04X}");
        let Some(bytes) = properties.get(&key).and_then(|value| value["hex"].as_str()) else {
            return (false, format!("Missing raw property {key}; no complete capture stored."));
        };
        if bytes.is_empty() || bytes.len() % 2 != 0 || bytes.len() > 2048
            || !bytes.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            return (false, format!("Property {key} had invalid or unbounded PTP bytes."));
        }
    }
    let Some(home) = std::env::var_os("HOME") else {
        return (false, "HOME unavailable; refusing to discard a completed capture.".into());
    };
    let output = std::path::PathBuf::from(home).join("Documents/FujiX Chromaforge/Camera Validation/Read-only C-slot Captures");
    if let Err(error) = std::fs::create_dir_all(&output) {
        return (false, format!("Cannot create safe capture directory: {error}"));
    }
    // Never put the camera's serial number in a filename or UI label.
    let _ = serial;
    if stdout.len() > 64 * 1024 { return (false, "Raw capture unexpectedly exceeded 64 KiB.".into()); }
    for number in 1..=10_000 {
        let filename = output.join(format!("X100VI-current-C{slot}-read-{number:04}.json"));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&filename) {
            Ok(mut f) => {
                if let Err(error) = f.write_all(stdout).and_then(|_| f.sync_all()) {
                    let _ = std::fs::remove_file(&filename);
                    return (false, format!("Could not preserve raw current-slot bytes: {error}"));
                }
                return (true, format!("Verified 26 read-only property records for CURRENT physical C{slot}; saved {}. This is one slot only—not a seven-slot backup or restore test.", filename.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return (false, format!("Current-slot capture could not be saved: {error}")),
        }
    }
    (false, "Too many captures in the destination folder.".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn classify_raw_mode(stdout: &[u8], exited_ok: bool) -> (bool, String) {
    let Ok(data) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return (false, "X100VI RAW bridge did not return a valid capability report.".into());
    };
    if !exited_ok || data["ok"] != true || data["device"] != "X100VI" || data["read_only_handshake"] != true
        || data["usb_vid"] != "04cb" || data["usb_pid"] != "0305" || data["conversion_available"] != false {
        let code = data["error"].as_str().unwrap_or("no verified X100VI device");
        return (false, format!("X100VI RAW mode not verified: {}. Offline previews remain available.",
            code.chars().take(120).collect::<String>()));
    }
    match data["usb_mode"].as_u64() {
        Some(6) if data["raw_conversion_mode"] == true =>
            (true, "Verified X100VI in USB RAW CONV. mode 6. Camera-rendered RAF conversion is still disabled pending validated upload/output transactions.".into()),
        Some(mode) => (true, format!("Verified X100VI USB mode {mode}; select USB RAW CONV./BACKUP RESTORE for camera-assisted RAW processing when that transport is implemented.")),
        None => (false, "X100VI USB mode property not provided by the verified camera bridge.".into()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn classify_current_slot(stdout: &[u8], exit_ok: bool) -> (bool, String, Option<String>) {
    let Ok(data) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return (false, "Native read did not return a valid Fujifilm camera report. Nothing was written.".into(), None);
    };
    if !exit_ok || data["ok"] != true {
        let reason = data["error"].as_str().unwrap_or("Camera read failed or was refused by macOS");
        return (false, format!("X100VI current-slot read unavailable: {}", reason.chars().take(220).collect::<String>()), None);
    }
    if data["model"] != "X100VI" || data["onlyCurrentSlot"] != true || data["writeCommands"] != false {
        return (false, "Camera helper did not prove X100VI and read-only single-slot mode.".into(), None);
    }
    let Some(slot) = data["activeSlot"].as_u64().filter(|v| (1..=7).contains(v)) else {
        return (false, "The camera responded, but its active C1–C7 selector was not readable. No backup was claimed.".into(), None);
    };
    let name = data["slotName"].as_str().unwrap_or("Unnamed preset").chars().take(80).collect::<String>();
    let props = data["properties"].as_object().map_or(0, |o| o.len());
    let film_code = data["properties"]["D192"]["value"].as_u64();
    let film = film_code.map_or("unknown".to_string(), |code| format!("0x{code:04X}"));
    let summary = format!("Current physical camera C{slot}: {name}\nFilm simulation raw value: {film} · {props} readable property records. No camera setting was changed.");
    (true, format!("Read-only X100VI current C{slot} inspected successfully; camera session closed. NOT a full C1–C7 backup."), Some(summary))
}

#[cfg(not(target_arch = "wasm32"))]
fn classify_probe(stdout: &[u8], stderr: &[u8], exit_ok: bool) -> (bool, String) {
    let out = String::from_utf8_lossy(stdout);
    let model = out.contains("ImageCaptureCore detected X100VI USB VID=0x04CB PID=0x0305");
    let device_info = out.contains("PTP DeviceInfo:") && out.contains("PTP response code=0x2001");
    if model && device_info && exit_ok {
        return (true, "X100VI USB detected: camera model and PTP DeviceInfo verified. Read-only session closed.".into());
    }
    let diagnostic = if out.contains("Probe timeout") {
        "No eligible X100VI appeared before the camera probe timed out. Check the camera's USB mode and cable."
    } else if out.contains("canAcceptPTPCommands=NO") {
        "macOS can see the X100VI but does not permit this PTP interface in the current USB mode."
    } else if model {
        "X100VI USB identity detected, but the read-only PTP DeviceInfo check did not succeed."
    } else if out.trim().is_empty() {
        "Camera helper returned no X100VI information. Check USB connection and permissions."
    } else {
        "Camera connection is not yet verified as an X100VI with readable PTP device metadata."
    };
    let error = String::from_utf8_lossy(stderr);
    let suffix = error.trim().chars().take(180).collect::<String>();
    (false, if suffix.is_empty() { diagnostic.to_string() } else { format!("{diagnostic} {suffix}") })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_101_original_recipes_load() {
        let s = FujiState::load();
        assert_eq!(s.original.len(), 101);
        assert!(s.original.iter().any(|(_,r)| r.name == "Cinestill 800t"));
    }
    #[test]
    fn profile_folders_keep_all_source_backups_and_c1_through_c7_order() {
        let source = FujiState::load();
        let groups = source.original_backups();
        assert_eq!(groups.len(), 16);
        assert_eq!(groups.values().map(Vec::len).sum::<usize>(), 101);
        assert_eq!(groups.keys().copied().collect::<Vec<_>>(),
            (1..=15).chain(std::iter::once(86)).collect::<Vec<_>>());
        for number in 1..=14 {
            assert_eq!(groups[&number].iter().map(|entry| entry.0).collect::<Vec<_>>(),
                (1..=7).collect::<Vec<_>>(), "Backup {number} must retain C1–C7");
        }
        assert_eq!(groups[&15].iter().map(|entry| entry.0).collect::<Vec<_>>(), [1]);
        assert_eq!(groups[&86].iter().map(|entry| entry.0).collect::<Vec<_>>(), [1, 2]);
    }
    #[test]
    fn local_camera_backups_keep_seven_editable_slots_and_roundtrip() {
        let drafts = local_backup_drafts(3, 500);
        assert_eq!(drafts.len(), 7);
        assert_eq!(drafts.iter().map(|d| d.id.clone()).collect::<Vec<_>>(),
            (500..=506).map(|n| format!("custom-{n}")).collect::<Vec<_>>());
        assert!(drafts.iter().all(|d| d.camera.as_ref().is_some_and(|c| c.values.is_empty())));
        let bytes = serde_json::to_vec(&drafts).unwrap();
        let restored: Vec<RecipeDraft> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.iter().filter_map(|d| d.local_slot.map(|s| s.slot)).collect::<Vec<_>>(),
            (1..=7).collect::<Vec<_>>());
        assert!(restored.iter().all(|d| d.local_slot.is_some_and(|s| s.backup == 3)));
        let legacy = br#"{"id":"custom-old","name":"Legacy","text":"","camera":null}"#;
        assert!(serde_json::from_slice::<RecipeDraft>(legacy).unwrap().local_slot.is_none());
    }
    #[test]
    fn original_is_never_modified_by_draft_creation() {
        let mut s = FujiState::load();
        let original = s.original[0].1.clone();
        s.select(&original.id);
        s.name.push_str(" modified");
        assert_eq!(s.original[0].1.name, original.name);
    }
    #[test]
    fn recipe_filter_searches_original_settings() {
        assert!(contains("chrome", "Any", "Film Simulation: Classic Chrome"));
        assert!(!contains("Acros", "Cine", "Film Simulation: Classic Chrome"));
    }
    #[test]
    fn original_and_draft_have_distinct_preset_panel_save_actions() {
        let mut state = FujiState::load();
        let original_id = state.original[0].1.id.clone();
        state.select(&original_id);
        assert!(state.original_selected());
        assert_eq!(state.recipe_kind(), "ORIGINAL · READ-ONLY SOURCE");
        assert_eq!(state.save_label(), "Save copy to My Recipes");

        state.drafts.push(RecipeDraft { id: "custom-test-preset-ui".into(),
            name: "My Custom X100VI".into(), text: "Film Simulation: Acros".into(), camera: None, local_slot: None, backup_label: None });
        state.select("custom-test-preset-ui");
        assert!(!state.original_selected());
        assert_eq!(state.recipe_kind(), "MY RECIPES · LOCAL DRAFT");
        assert_eq!(state.save_label(), "Save recipe changes");
        state.new_recipe();
        assert_eq!(state.recipe_kind(), "NEW LOCAL RECIPE");
        assert_eq!(state.save_label(), "Create recipe in My Recipes");
    }
    #[test]
    fn selecting_real_source_populates_camera_controls_and_forks_original_on_edit() {
        let mut s = FujiState::load();
        let recipe = s.original.iter().find(|(_, r)| r.name == "Cinestill 800t").unwrap().1.clone();
        s.select(&recipe.id);
        assert_eq!(s.camera.get("film_simulation"), "Eterna");
        assert_eq!(s.camera.get("grain_strength"), "Strong");
        assert_eq!(s.camera.get("grain_size"), "Large");
        assert_eq!(s.camera.number("highlight"), Some(0.0));
        assert_eq!(s.camera.number("color"), Some(4.0));
        let original = s.original.iter().find(|(_, r)| r.id == recipe.id).unwrap().1.raw_text.clone();
        s.camera.set("grain_strength", "Weak");
        changed(&mut s);
        assert_eq!(s.camera.get("grain_strength"), "Weak");
        assert_eq!(s.original.iter().find(|(_, r)| r.id == recipe.id).unwrap().1.raw_text, original);
        let draft = RecipeDraft { id: "custom-test".into(), name: s.name.clone(), text: s.text.clone(), camera: Some(s.camera.clone()), local_slot: None, backup_label: None };
        let restored: RecipeDraft = serde_json::from_slice(&serde_json::to_vec(&draft).unwrap()).unwrap();
        assert_eq!(restored.camera.unwrap().get("grain_strength"), "Weak");
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_camera_probe_requires_identical_model_and_ptp_success() {
        let ok = b"ImageCaptureCore detected X100VI USB VID=0x04CB PID=0x0305\nPTP response code=0x2001\nPTP DeviceInfo: std_version=0x0100";
        assert!(classify_probe(ok, b"", true).0);
        assert!(!classify_probe(ok, b"", false).0);
        assert!(!classify_probe(b"ImageCaptureCore detected X100VI USB VID=0x04CB PID=0x0305\nPTP response code=0x2005", b"", true).0);
        assert!(!classify_probe(b"PTP response code=0x2001\nPTP DeviceInfo:", b"", true).0);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn current_slot_helper_report_must_prove_readonly_mode_and_active_slot() {
        let good = br#"{"ok":true,"model":"X100VI","onlyCurrentSlot":true,"writeCommands":false,"activeSlot":3,"slotName":"My Classic Chrome","properties":{"D18C":{"value":3},"D192":{"value":4}}}"#;
        assert!(classify_current_slot(good, true).0);
        assert!(classify_current_slot(good, true).2.unwrap_or_default().contains("C3"));
        assert!(!classify_current_slot(good, false).0);
        let mut wrong: serde_json::Value = serde_json::from_slice(good).unwrap_or_default();
        wrong["activeSlot"] = serde_json::json!(9);
        assert!(!classify_current_slot(&serde_json::to_vec(&wrong).unwrap_or_default(), true).0);
        let write = br#"{"ok":true,"model":"X100VI","onlyCurrentSlot":true,"writeCommands":true,"activeSlot":3,"slotName":"Bad"}"#;
        assert!(!classify_current_slot(write, true).0);
    }
    #[test]
    fn offline_backup_roundtrip_is_strict_and_preserves_seven_slots() {
        let mut state = FujiState::load();
        state.drafts = local_backup_drafts(2, 200);
        for draft in &mut state.drafts { draft.backup_label = Some("Color Experiments".into()); }
        state.drafts[1].name = "My Acros".into();
        state.drafts[1].camera.as_mut().unwrap().set("film_simulation", "Acros");
        let package = state.package_local_backup(2).unwrap();
        assert_eq!(package.name, "Color Experiments");
        assert_eq!(package.slots.len(), 7);
        assert_eq!(package.slots[1].slot, 2);
        assert_eq!(package.slots[1].camera.get("film_simulation"), "Acros");
        let copy: OfflineBackupPackage = serde_json::from_slice(&serde_json::to_vec(&package).unwrap()).unwrap();
        copy.validate().unwrap();
        let mut wrong = copy.clone();
        wrong.slots[6].slot = 2;
        assert!(wrong.validate().is_err(), "duplicate slot C2 must be refused");
        let mut wrong = copy.clone();
        wrong.kind = "fujifilm-camera-settings-backup".into();
        assert!(wrong.validate().is_err(), "untrusted formats must be refused");
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn raw_mode_report_never_claims_conversion_availability() {
        let report = br#"{"ok":true,"device":"X100VI","usb_vid":"04cb","usb_pid":"0305","usb_mode":6,"raw_conversion_mode":true,"read_only_handshake":true,"conversion_available":false}"#;
        assert!(classify_raw_mode(report, true).0);
        let mut bad: serde_json::Value = serde_json::from_slice(report).unwrap();
        bad["conversion_available"] = serde_json::json!(true);
        assert!(!classify_raw_mode(&serde_json::to_vec(&bad).unwrap(), true).0);
        assert!(!classify_raw_mode(report, false).0);
    }
}
