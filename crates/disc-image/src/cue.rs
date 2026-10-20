//! Minimal CUE sheet parser for binary CD images.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

#[derive(Clone, Copy)]
pub(crate) struct Msf {
    m: u8,
    s: u8,
    f: u8,
}

impl Msf {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let m = parts.next()?.trim().parse().ok()?;
        let s = parts.next()?.trim().parse().ok()?;
        let f = parts.next()?.trim().parse().ok()?;
        if parts.next().is_none() && s < 60 && f < 75 {
            Some(Self { m, s, f })
        } else {
            None
        }
    }

    pub(crate) fn frames(self) -> u64 {
        (u64::from(self.m) * 60 + u64::from(self.s)) * 75 + u64::from(self.f)
    }
}

#[derive(Clone)]
pub(crate) struct Index {
    pub(crate) number: u32,
    pub(crate) time: Msf,
}

#[derive(Clone)]
pub(crate) struct Track {
    pub(crate) number: u32,
    pub(crate) mode: String,
    pub(crate) indices: Vec<Index>,
}

impl Track {
    pub(crate) fn earliest_index(&self) -> Option<Msf> {
        self.indices
            .iter()
            .map(|index| index.time)
            .min_by_key(|time| time.frames())
    }
}

#[derive(Clone)]
pub(crate) struct MediaFile {
    path: PathBuf,
    pub(crate) file_type: String,
    pub(crate) tracks: Vec<Track>,
}

impl MediaFile {
    pub(crate) fn resolve_path(&self, cue_path: &Path) -> PathBuf {
        cue_path.parent().unwrap_or(Path::new(".")).join(&self.path)
    }
}

#[derive(Clone)]
pub(crate) struct CueSheet {
    pub(crate) files: Vec<MediaFile>,
}

impl CueSheet {
    /// Parse CUE directives while retaining native `FILE` filename bytes.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut files = Vec::new();

        for raw in bytes.split(|byte| *byte == b'\n' || *byte == b'\r') {
            let line = trim_ascii(raw);
            if line.is_empty() {
                continue;
            }
            let split = line
                .iter()
                .position(|byte| byte.is_ascii_whitespace())
                .unwrap_or(line.len());
            let (command, tail) = line.split_at(split);
            let rest = trim_ascii(tail);
            if command.eq_ignore_ascii_case(b"REM") {
                continue;
            }
            let command = ascii_text(command, "CUE directive")?;
            match command.to_ascii_uppercase().as_str() {
                "FILE" => {
                    let (path, file_type) = split_file(rest)?;
                    if path.as_os_str().is_empty() {
                        return Err("FILE requires a media filename".into());
                    }
                    files.push(MediaFile {
                        path,
                        file_type: file_type.unwrap_or_default(),
                        tracks: Vec::new(),
                    });
                }
                "TRACK" => {
                    let mut fields = ascii_text(rest, "TRACK fields")?.split_whitespace();
                    let number_text = fields.next().ok_or("TRACK requires a number and mode")?;
                    let number = number_text
                        .parse()
                        .map_err(|_| format!("bad TRACK number: {number_text}"))?;
                    let mode = fields
                        .next()
                        .ok_or("TRACK requires a number and mode")?
                        .to_owned();
                    let file = files.last_mut().ok_or("TRACK before FILE")?;
                    file.tracks.push(Track {
                        number,
                        mode,
                        indices: Vec::new(),
                    });
                }
                "INDEX" => {
                    let mut fields = ascii_text(rest, "INDEX fields")?.split_whitespace();
                    let number_text = fields.next().ok_or("INDEX requires a number and time")?;
                    let number = number_text
                        .parse()
                        .map_err(|_| format!("bad INDEX number: {number_text}"))?;
                    let time_text = fields.next().ok_or("INDEX requires a number and time")?;
                    let time = Msf::parse(time_text)
                        .ok_or_else(|| format!("bad INDEX time: {time_text}"))?;
                    Self::last_track(&mut files)?
                        .indices
                        .push(Index { number, time });
                }
                "FLAGS" => {
                    Self::last_track(&mut files)?;
                    ascii_text(rest, "FLAGS fields")?;
                }
                "PREGAP" | "POSTGAP" | "CATALOG" | "CDTEXTFILE" | "TITLE" | "PERFORMER"
                | "SONGWRITER" | "ISRC" => {}
                other => return Err(format!("unknown CUE directive: {other}")),
            }
        }

        if files.is_empty() || files.iter().all(|file| file.tracks.is_empty()) {
            return Err("CUE sheet contains no TRACK".into());
        }
        for track in files.iter().flat_map(|file| &file.tracks) {
            if !track.indices.iter().any(|index| index.number == 1) {
                return Err(format!("TRACK {:02} has no INDEX 01", track.number));
            }
        }
        Ok(Self { files })
    }

    fn last_track(files: &mut [MediaFile]) -> Result<&mut Track, String> {
        files
            .last_mut()
            .and_then(|file| file.tracks.last_mut())
            .ok_or_else(|| "INDEX/FLAGS before TRACK".to_string())
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn ascii_text<'a>(bytes: &'a [u8], field: &str) -> Result<&'a str, String> {
    if !bytes.is_ascii() {
        return Err(format!("{field} must be ASCII"));
    }
    std::str::from_utf8(bytes).map_err(|_| format!("{field} must be ASCII"))
}

fn split_file(rest: &[u8]) -> Result<(PathBuf, Option<String>), String> {
    let rest = trim_ascii(rest);
    let (name, type_bytes) = if let Some(stripped) = rest.strip_prefix(b"\"") {
        match stripped.iter().position(|byte| *byte == b'\"') {
            Some(end) => {
                let tail = trim_ascii(&stripped[end + 1..]);
                let split = tail
                    .iter()
                    .position(|byte| byte.is_ascii_whitespace())
                    .unwrap_or(tail.len());
                (&stripped[..end], (!tail.is_empty()).then(|| &tail[..split]))
            }
            None => (stripped, None),
        }
    } else {
        let split = rest
            .iter()
            .position(|byte| byte.is_ascii_whitespace())
            .unwrap_or(rest.len());
        (
            &rest[..split],
            (split < rest.len()).then(|| trim_ascii(&rest[split..])),
        )
    };
    let file_type = type_bytes
        .filter(|tail| !tail.is_empty())
        .map(|tail| ascii_text(tail, "FILE type").map(str::to_owned))
        .transpose()?;
    Ok((filename_path(name)?, file_type))
}

#[cfg(unix)]
fn filename_path(bytes: &[u8]) -> Result<PathBuf, String> {
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn filename_path(bytes: &[u8]) -> Result<PathBuf, String> {
    std::str::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|_| "CUE media filename is not valid UTF-8 on this platform".into())
}
