use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use disc_image::DiscImage;
use iso9660::{DirEntry, ISO_SECTOR, SectorSource, read_root};
use md5::{Digest, Md5};

use crate::{GameInfo, HashError, serial};

const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Adapts a [`DiscImage`] to [`SectorSource`]; the orphan rule forbids a direct
/// impl here because both the trait and the type are foreign to this crate.
struct DiscSectors(DiscImage);

impl SectorSource for DiscSectors {
    fn read_lba(&mut self, lba: u64) -> io::Result<[u8; ISO_SECTOR]> {
        self.0.read_lba(lba)
    }

    fn stream_extent<F>(&mut self, lba: u64, size: u64, feed: F) -> io::Result<u64>
    where
        F: FnMut(&[u8]),
    {
        self.0.stream_extent(lba, size, feed)
    }
}

type TargetedIso<'a> = iso9660::TargetedIso<'a, DiscSectors>;

enum Platform {
    PlayStation,
    PlayStation2,
    PlayStationPortable,
}

impl Platform {
    fn info(self, identity_md5: String, serial: Option<String>) -> GameInfo {
        let (platform, retroachievements_platform_id) = match self {
            Self::PlayStation => ("ps1", 12),
            Self::PlayStation2 => ("ps2", 21),
            Self::PlayStationPortable => ("psp", 41),
        };
        GameInfo {
            platform,
            serial,
            identity_md5,
            retroachievements_platform_id,
        }
    }
}

pub(crate) fn generate_hash(input: &Path) -> Result<String, HashError> {
    Ok(inspect(input, false)?.identity_md5)
}

pub(crate) fn game_info(input: &Path) -> Result<GameInfo, HashError> {
    inspect(input, true)
}

fn inspect(input: &Path, include_serial: bool) -> Result<GameInfo, HashError> {
    if probe_pbp(input)? {
        let identity_md5 = hash_pbp(input)?;
        let serial = include_serial.then(|| serial::pbp_serial(input)).flatten();
        return Ok(Platform::PlayStationPortable.info(identity_md5, serial));
    }

    let image = DiscImage::open(input).map_err(|error| HashError::new(error.to_string()))?;
    let mut disc = DiscSectors(image);
    let root = read_root(&mut disc).map_err(HashError::new)?;
    let Some(root) = root else {
        return unavailable("optical input has no ISO 9660 filesystem");
    };
    let mut iso = TargetedIso::new(&mut disc, root);
    let platform = detect_platform(&mut iso)?;
    let (identity_md5, serial) = match platform {
        Platform::PlayStation => hash_ps1(&mut iso, include_serial)?,
        Platform::PlayStation2 => hash_ps2(&mut iso, include_serial)?,
        Platform::PlayStationPortable => {
            let identity_md5 = hash_psp_iso(&mut iso)?;
            let serial = if include_serial {
                psp_serial(&mut iso)
            } else {
                None
            };
            (identity_md5, serial)
        }
    };
    Ok(platform.info(identity_md5, serial))
}

fn probe_pbp(path: &Path) -> Result<bool, HashError> {
    let mut file = File::open(path).map_err(|error| {
        HashError::new(format!("cannot open input {}: {error}", path.display()))
    })?;
    let mut magic = [0; 4];
    let read = file
        .read(&mut magic)
        .map_err(|error| HashError::new(error.to_string()))?;
    if read == magic.len() && magic == *b"\0PBP" {
        return Ok(true);
    }
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pbp"))
    {
        return Err(HashError::new(format!(
            "{} has a .pbp extension but no \\0PBP magic at byte 0",
            path.display()
        )));
    }
    Ok(false)
}

fn detect_platform(iso: &mut TargetedIso<'_>) -> Result<Platform, HashError> {
    if validate_psp(iso).unwrap_or(false) {
        return Ok(Platform::PlayStationPortable);
    }
    let ps2 = validate_ps2(iso).unwrap_or(false);
    let ps1 = validate_ps1(iso).unwrap_or(false);
    match (ps1, ps2) {
        (true, false) => Ok(Platform::PlayStation),
        (false, true) => Ok(Platform::PlayStation2),
        (true, true) => Err(HashError::new(
            "platform evidence is ambiguous across PlayStation and PlayStation 2",
        )),
        (false, false) => unavailable("platform is unknown"),
    }
}

fn validate_ps1(iso: &mut TargetedIso<'_>) -> Result<bool, HashError> {
    if let Some(cnf) = system_cnf(iso)?
        && cnf.get("BOOT").is_some()
    {
        return Ok(true);
    }
    let Some(entry) = iso.find_file("PSX.EXE").map_err(HashError::new)? else {
        return Ok(false);
    };
    Ok(iso
        .read_entry(&entry, entry.size.min(8))
        .map_err(|error| HashError::new(format!("cannot read {}: {error}", entry.name)))?
        .as_slice()
        == b"PS-X EXE")
}

fn validate_ps2(iso: &mut TargetedIso<'_>) -> Result<bool, HashError> {
    Ok(system_cnf(iso)?.is_some_and(|cnf| cnf.get("BOOT2").is_some()))
}

fn validate_psp(iso: &mut TargetedIso<'_>) -> Result<bool, HashError> {
    Ok(iso
        .find_file("PSP_GAME/PARAM.SFO")
        .map_err(HashError::new)?
        .is_some()
        && iso
            .find_file("PSP_GAME/SYSDIR/EBOOT.BIN")
            .map_err(HashError::new)?
            .is_some())
}

fn hash_ps1(
    iso: &mut TargetedIso<'_>,
    include_serial: bool,
) -> Result<(String, Option<String>), HashError> {
    let boot_target = match system_cnf(iso)? {
        Some(cnf) => match cnf.boot_target("BOOT") {
            Some(target) => iso
                .find_file(&target.lookup_name)
                .map_err(HashError::new)?
                .map(|entry| (target.identity_name, entry, true)),
            None => None,
        },
        None => None,
    };
    let target = match boot_target {
        Some(target) => Some(target),
        None => iso
            .find_file("PSX.EXE")
            .map_err(HashError::new)?
            .map(|entry| ("PSX.EXE".to_owned(), entry, false)),
    };
    let Some((identity_name, entry, from_boot)) = target else {
        return unavailable("could not resolve the BOOT target or root PSX.EXE");
    };
    let prefix = read_exact(iso, &entry, 32)?;
    let selected = if prefix.starts_with(b"PS-X EXE") {
        u64::from(u32::from_le_bytes([
            prefix[0x1c],
            prefix[0x1d],
            prefix[0x1e],
            prefix[0x1f],
        ]))
        .saturating_add(2048)
        .min(MAX_INPUT_BYTES)
    } else {
        entry.size.min(MAX_INPUT_BYTES)
    };
    let identity_md5 = hash_named_entry(iso, &identity_name, &entry, selected)?;
    let serial = (include_serial && from_boot)
        .then(|| serial::console_serial(&identity_name))
        .flatten();
    Ok((identity_md5, serial))
}

fn hash_ps2(
    iso: &mut TargetedIso<'_>,
    include_serial: bool,
) -> Result<(String, Option<String>), HashError> {
    let Some(cnf) = system_cnf(iso)? else {
        return unavailable("SYSTEM.CNF is unavailable");
    };
    let Some(target) = cnf.boot_target("BOOT2") else {
        return unavailable("SYSTEM.CNF has no valid BOOT2 cdrom0: target");
    };
    let Some(entry) = iso.find_file(&target.lookup_name).map_err(HashError::new)? else {
        return unavailable(format!(
            "BOOT2 target {} does not resolve to a file",
            target.identity_name
        ));
    };
    let selected = entry.size.min(MAX_INPUT_BYTES);
    let identity_md5 = hash_named_entry(iso, &target.identity_name, &entry, selected)?;
    let serial = include_serial
        .then(|| serial::console_serial(&target.identity_name))
        .flatten();
    Ok((identity_md5, serial))
}

fn hash_named_entry(
    iso: &mut TargetedIso<'_>,
    identity_name: &str,
    entry: &DirEntry,
    selected: u64,
) -> Result<String, HashError> {
    let mut md5 = Md5::new();
    md5.update(identity_name.as_bytes());
    iso.stream_entry(entry, selected, |bytes| md5.update(bytes))
        .map_err(|error| {
            HashError::new(format!(
                "cannot supply all {selected} selected bytes from {identity_name}: {error}"
            ))
        })?;
    Ok(hex::encode(md5.finalize()))
}

fn hash_psp_iso(iso: &mut TargetedIso<'_>) -> Result<String, HashError> {
    let Some(param) = iso
        .find_file("PSP_GAME/PARAM.SFO")
        .map_err(HashError::new)?
    else {
        return unavailable("PSP_GAME/PARAM.SFO is unavailable");
    };
    let Some(eboot) = iso
        .find_file("PSP_GAME/SYSDIR/EBOOT.BIN")
        .map_err(HashError::new)?
    else {
        return unavailable("PSP_GAME/SYSDIR/EBOOT.BIN is unavailable");
    };
    let mut md5 = Md5::new();
    stream_psp_entry(
        iso,
        &param,
        param.size.min(MAX_INPUT_BYTES),
        "PARAM.SFO",
        &mut md5,
    )?;
    stream_psp_entry(
        iso,
        &eboot,
        eboot.size.min(MAX_INPUT_BYTES),
        "EBOOT.BIN",
        &mut md5,
    )?;
    Ok(hex::encode(md5.finalize()))
}

fn psp_serial(iso: &mut TargetedIso<'_>) -> Option<String> {
    let param = iso.find_file("PSP_GAME/PARAM.SFO").ok()??;
    let data = iso
        .read_entry(&param, param.size.min(MAX_INPUT_BYTES))
        .ok()?;
    serial::sfo_serial(&data)
}

fn stream_psp_entry(
    iso: &mut TargetedIso<'_>,
    entry: &DirEntry,
    selected: u64,
    label: &str,
    md5: &mut Md5,
) -> Result<(), HashError> {
    iso.stream_entry(entry, selected, |bytes| md5.update(bytes))
        .map_err(|error| {
            HashError::new(format!("cannot supply all selected {label} bytes: {error}"))
        })?;
    Ok(())
}

fn hash_pbp(path: &Path) -> Result<String, HashError> {
    let size = std::fs::metadata(path)
        .map_err(|error| HashError::new(error.to_string()))?
        .len();
    let selected = size.min(MAX_INPUT_BYTES);
    let mut file = File::open(path).map_err(|error| {
        HashError::new(format!("cannot reopen PBP for identity hashing: {error}"))
    })?;
    let mut md5 = Md5::new();
    let mut remaining = selected;
    let mut buffer = [0; 64 * 1024];
    while remaining != 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..wanted])
            .map_err(|error| HashError::new(format!("cannot read selected PBP prefix: {error}")))?;
        if read == 0 {
            return unavailable(format!(
                "PBP ended before all {selected} selected bytes were available"
            ));
        }
        md5.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hex::encode(md5.finalize()))
}

fn read_exact(
    iso: &mut TargetedIso<'_>,
    entry: &DirEntry,
    size: u64,
) -> Result<Vec<u8>, HashError> {
    if entry.size < size {
        return unavailable(format!(
            "{} is only {} bytes; {size} bytes are required for identity selection",
            entry.name, entry.size
        ));
    }
    iso.read_entry(entry, size).map_err(|error| {
        HashError::new(format!(
            "cannot read identity-selection prefix from {}: {error}",
            entry.name
        ))
    })
}

fn unavailable<T>(message: impl Into<String>) -> Result<T, HashError> {
    Err(HashError::new(format!(
        "RetroAchievements-compatible hash unavailable: {}",
        message.into()
    )))
}

struct SystemCnf {
    entries: Vec<(String, String)>,
}

impl SystemCnf {
    fn parse(data: &[u8]) -> Self {
        let mut entries = Vec::new();
        for line in String::from_utf8_lossy(data).lines() {
            let line = line.trim().trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                entries.push((key.trim().to_ascii_uppercase(), value.trim().to_owned()));
            }
        }
        Self { entries }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    fn boot_target(&self, key: &str) -> Option<BootTarget> {
        let (prefix, prefix_required) = match key {
            "BOOT" => ("cdrom:", false),
            "BOOT2" => ("cdrom0:", true),
            _ => return None,
        };
        let value = self.get(key)?.trim();
        let value = match value.strip_prefix(prefix) {
            Some(value) => value,
            None if !prefix_required => value,
            None => return None,
        };
        let identity_name = value
            .trim_start_matches(['\\', '/'])
            .split(|character: char| character.is_whitespace() || character == ';')
            .next()?;
        if identity_name.is_empty() {
            return None;
        }
        Some(BootTarget {
            lookup_name: identity_name.to_ascii_uppercase(),
            identity_name: identity_name.to_owned(),
        })
    }
}

struct BootTarget {
    lookup_name: String,
    identity_name: String,
}

fn system_cnf(iso: &mut TargetedIso<'_>) -> Result<Option<SystemCnf>, HashError> {
    let Some(entry) = iso.find_file("SYSTEM.CNF").map_err(HashError::new)? else {
        return Ok(None);
    };
    let data = iso
        .read_entry(&entry, entry.size.min(2048))
        .map_err(|error| HashError::new(format!("cannot read SYSTEM.CNF: {error}")))?;
    Ok(Some(SystemCnf::parse(&data)))
}
