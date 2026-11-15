//! Source-side X100VI RAF integrity evidence. This does not open USB or issue
//! camera commands, and explicitly does *not* certify a reversible camera
//! transaction. A real 7-slot camera backup and JPEG round trip are separate
//! gates and must never be inferred from this file-level check.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};

#[derive(Clone, Serialize)]
pub(super) struct RafPreflightEvidence {
    pub kind: &'static str,
    pub version: u32,
    pub source_name: String,
    pub source_model_from_header: &'static str,
    pub raf_size: u64,
    pub raf_sha256: String,
    pub repeated_hash_agrees: bool,
    pub status: &'static str,
    pub usb_transmits: bool,
    pub camera_verified: bool,
    pub physical_c1_c7_backed_up: bool,
    pub camera_rendered_jpeg_verified: bool,
    pub physical_slot_restore_verified: bool,
}

fn read_hash(path: &Path, expected_size: u64) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Could not open RAF for reading: {e}"))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| format!("Unable to read original RAF: {e}"))?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
        total = total.checked_add(count as u64).ok_or("Invalid RAF length")?;
        if total > expected_size { return Err("RAF grew during source verification".into()); }
    }
    if total != expected_size { return Err("RAF size changed during source verification".into()); }
    // sha2's GenericArray has no LowerHex implementation in the exact
    // workspace version. Encode the fixed 32-byte digest without `hex` deps.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for value in digest.finalize() {
        encoded.push(HEX[(value >> 4) as usize] as char);
        encoded.push(HEX[(value & 0x0F) as usize] as char);
    }
    Ok(encoded)
}

fn verify(original_path: &str) -> Result<RafPreflightEvidence, String> {
    let path = Path::new(original_path.trim());
    if !path.is_absolute() || !path.extension().and_then(|v| v.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("raf")) {
        return Err("Choose an absolute path to a local .RAF file".into());
    }
    let metadata = fs::metadata(path).map_err(|e| format!("RAF unavailable: {e}"))?;
    if !metadata.is_file() || metadata.len() < 64 || metadata.len() > u64::from(u32::MAX) {
        return Err("RAF must be a regular file between 64 bytes and 4 GiB".into());
    }
    let mut f = File::open(path).map_err(|e| format!("Unable to read RAF header: {e}"))?;
    let mut header = [0u8; 64];
    f.read_exact(&mut header).map_err(|e| format!("Unable to read complete RAF header: {e}"))?;
    if !header.starts_with(b"FUJIFILMCCD-RAW") {
        return Err("Input has no Fujifilm RAF header; refusing camera transfer".into());
    }
    if !header.windows(6).any(|v| v == b"X100VI") {
        return Err("RAF header does not identify model X100VI".into());
    }
    let initial_time = metadata.modified().ok();
    let first_hash = read_hash(path, metadata.len())?;
    let repeated_hash = read_hash(path, metadata.len())?;
    let after = fs::metadata(path).map_err(|e| format!("RAF became inaccessible: {e}"))?;
    if metadata.len() != after.len() || initial_time != after.modified().ok() || first_hash != repeated_hash {
        return Err("RAF changed or failed repeat SHA-256 verification; no camera transfer permitted".into());
    }
    let source_name = path.file_name().and_then(|s| s.to_str()).ok_or("RAF file name is invalid")?.to_owned();
    Ok(RafPreflightEvidence {
        kind: "fujix-raf-source-audit",
        version: 1,
        source_name,
        source_model_from_header: "X100VI",
        raf_size: metadata.len(),
        raf_sha256: first_hash,
        repeated_hash_agrees: true,
        status: "offline_source_integrity_only",
        usb_transmits: false,
        camera_verified: false,
        physical_c1_c7_backed_up: false,
        camera_rendered_jpeg_verified: false,
        physical_slot_restore_verified: false,
    })
}

pub(super) fn inspect_and_save(original_path: &str) -> Result<(RafPreflightEvidence, PathBuf), String> {
    let audit = verify(original_path)?;
    let home = std::env::var_os("HOME").ok_or("HOME is not available")?;
    let folder = PathBuf::from(home).join("Documents/FujiX Chromaforge/Camera Validation");
    fs::create_dir_all(&folder).map_err(|e| format!("Cannot create audit folder: {e}"))?;
    let name: String = audit.source_name.chars()
        .filter_map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' { Some(ch) }
            else if ch == ' ' || ch == '_' || ch == '.' { Some('-') } else { None })
        .take(48).collect();
    let prefix = if name.is_empty() { "X100VI-RAF" } else { &name };
    let payload = serde_json::to_vec_pretty(&audit).map_err(|e| e.to_string())?;
    for n in 0..10_000 {
        let filename = folder.join(if n == 0 {
            format!("{prefix}-{}-source-audit.json", &audit.raf_sha256[..12])
        } else {
            format!("{prefix}-{}-source-audit-{n}.json", &audit.raf_sha256[..12])
        });
        match OpenOptions::new().write(true).create_new(true).open(&filename) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&payload).and_then(|_| f.sync_all()) {
                    let _ = fs::remove_file(&filename);
                    return Err(format!("Unable to save RAF integrity report: {e}"));
                }
                return Ok((audit, filename));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Unable to create RAF integrity report: {e}")),
        }
    }
    Err("Too many reports for the same RAF".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_file_does_not_pass_as_a_camera_raf() {
        assert!(verify("relative-file.raf").is_err());
        assert!(verify("/definitely-not-present-x100vi-file.RAF").is_err());
    }
}
