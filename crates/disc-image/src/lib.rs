//! Optical disc image opening and 2048-byte user-data sector access.
//!
//! [`DiscImage::open`] accepts a CUE sheet (selecting its first data track,
//! which must be `BINARY` media) or a directly supplied ISO/BIN/MDF image,
//! whose sector layout is inferred from sync bytes and the ISO 9660 signature.
//! Raw 2352/2448/2336-byte sectors are addressed by their user data only.

mod cue;

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::cue::{CueSheet, Track};

/// A CUE sheet, image layout, or image file access failure.
#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// User-data bytes per sector, as addressed by [`DiscImage::read_lba`].
pub const ISO_SECTOR: usize = 2048;
const RAW_SECTOR: usize = 2352;
const SUBCHANNEL_SECTOR: usize = 2448;
const SYNC: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

#[derive(Clone, Copy)]
struct Layout {
    sector_size: usize,
    data_offset: usize,
    has_user_data: bool,
}

fn layout_for_mode(mode: &str) -> Result<Layout, Error> {
    let layout = match mode.to_ascii_uppercase().as_str() {
        "MODE2/2352" => Layout {
            sector_size: 2352,
            data_offset: 24,
            has_user_data: true,
        },
        "MODE2/2448" => Layout {
            sector_size: SUBCHANNEL_SECTOR,
            data_offset: 24,
            has_user_data: true,
        },
        "MODE1/2352" => Layout {
            sector_size: 2352,
            data_offset: 16,
            has_user_data: true,
        },
        "MODE1/2448" => Layout {
            sector_size: SUBCHANNEL_SECTOR,
            data_offset: 16,
            has_user_data: true,
        },
        "MODE2/2336" => Layout {
            sector_size: 2336,
            data_offset: 8,
            has_user_data: true,
        },
        "MODE1/2336" => Layout {
            sector_size: 2336,
            data_offset: 0,
            has_user_data: true,
        },
        "MODE1/2048" | "CDROM/2048" => Layout {
            sector_size: ISO_SECTOR,
            data_offset: 0,
            has_user_data: true,
        },
        "AUDIO" => Layout {
            sector_size: RAW_SECTOR,
            data_offset: 0,
            has_user_data: false,
        },
        _ => {
            return Err(Error::new(format!("unsupported CUE track mode {mode:?}")));
        }
    };
    Ok(layout)
}

#[derive(Clone, Copy)]
struct TrackByteRange {
    layout: Layout,
    stored_start: u64,
    program_start: u64,
    end: u64,
}

fn track_byte_ranges(tracks: &[Track], file_size: u64) -> Result<Vec<TrackByteRange>, Error> {
    let mut ranges = Vec::with_capacity(tracks.len());
    let mut previous_frame = 0u64;
    let mut previous_sector_size = 0u64;
    let mut stored_start = 0u64;

    for (position, track) in tracks.iter().enumerate() {
        let layout = layout_for_mode(&track.mode)?;
        let stored_frame = track
            .earliest_index()
            .ok_or_else(|| Error::new(format!("TRACK {:02} has no INDEX", track.number)))?
            .frames();
        if position == 0 {
            stored_start = stored_frame
                .checked_mul(layout.sector_size as u64)
                .ok_or_else(|| Error::new("CUE track offset overflow"))?;
        } else {
            let frames = stored_frame.checked_sub(previous_frame).ok_or_else(|| {
                Error::new(format!(
                    "TRACK {:02} begins before the preceding track",
                    track.number
                ))
            })?;
            stored_start = stored_start
                .checked_add(
                    frames
                        .checked_mul(previous_sector_size)
                        .ok_or_else(|| Error::new("CUE track offset overflow"))?,
                )
                .ok_or_else(|| Error::new("CUE track offset overflow"))?;
        }
        let program_frame = track
            .indices
            .iter()
            .find(|index| index.number == 1)
            .expect("CUE parser requires INDEX 01")
            .time
            .frames();
        let program_frames = program_frame.checked_sub(stored_frame).ok_or_else(|| {
            Error::new(format!(
                "TRACK {:02} INDEX 01 precedes its stored pregap",
                track.number
            ))
        })?;
        let program_start = stored_start
            .checked_add(
                program_frames
                    .checked_mul(layout.sector_size as u64)
                    .ok_or_else(|| Error::new("CUE track offset overflow"))?,
            )
            .ok_or_else(|| Error::new("CUE track offset overflow"))?;
        ranges.push(TrackByteRange {
            layout,
            stored_start,
            program_start,
            end: 0,
        });
        previous_frame = stored_frame;
        previous_sector_size = layout.sector_size as u64;
    }

    for index in 0..ranges.len() {
        let end = ranges
            .get(index + 1)
            .map_or(file_size, |next| next.stored_start);
        let range = &mut ranges[index];
        let sector_size = range.layout.sector_size as u64;
        if range.stored_start > range.program_start
            || range.program_start > end
            || end > file_size
            || !(end - range.stored_start).is_multiple_of(sector_size)
        {
            return Err(Error::new(format!(
                "CUE track {} has an invalid or truncated byte range {}..{end} in a {file_size}-byte file",
                tracks[index].number, range.stored_start
            )));
        }
        range.end = end;
    }
    Ok(ranges)
}

/// An opened image restricted to its selected data track's user-data sectors.
pub struct DiscImage {
    image_path: PathBuf,
    layout: Layout,
    sector_count: u64,
    data_start: u64,
    file: File,
}

impl DiscImage {
    /// Open a CUE-described binary image or a directly supplied ISO/BIN/MDF.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable files, malformed or unsupported CUE
    /// sheets, unrecognizable layouts, or truncated/misaligned sector ranges.
    pub fn open(input: &Path) -> Result<Self, Error> {
        if input
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
        {
            let bytes = std::fs::read(input).map_err(|error| {
                Error::new(format!(
                    "cannot read CUE sheet {}: {error}",
                    input.display()
                ))
            })?;
            let cue = CueSheet::parse(&bytes).map_err(Error::new)?;
            let mut selected = None;
            for (media_index, media) in cue.files.iter().enumerate() {
                for (track_index, track) in media.tracks.iter().enumerate() {
                    let layout = layout_for_mode(&track.mode)?;
                    if layout.has_user_data {
                        selected = Some((media_index, track_index, layout));
                        break;
                    }
                }
                if selected.is_some() {
                    break;
                }
            }
            let (media_index, track_index, layout) =
                selected.ok_or_else(|| Error::new("CUE sheet contains no data TRACK"))?;
            let media = &cue.files[media_index];
            if !media.file_type.eq_ignore_ascii_case("BINARY") {
                return Err(Error::new(format!(
                    "unsupported CUE FILE type {:?}; data tracks require BINARY media",
                    media.file_type
                )));
            }
            let image = media.resolve_path(input);
            let size = std::fs::metadata(&image)
                .map_err(|error| Error::new(format!("cannot stat {}: {error}", image.display())))?
                .len();
            let range = track_byte_ranges(&media.tracks, size)?[track_index];
            Self::finish(
                &image,
                layout,
                Some((range.program_start, range.end - range.program_start)),
            )
        } else {
            let layout = Self::infer_layout(input)?;
            Self::finish(input, layout, None)
        }
    }

    fn finish(
        image_path: &Path,
        layout: Layout,
        data_range: Option<(u64, u64)>,
    ) -> Result<Self, Error> {
        let file = File::open(image_path).map_err(|error| {
            Error::new(format!(
                "cannot open image {}: {error}",
                image_path.display()
            ))
        })?;
        let size = file
            .metadata()
            .map_err(|error| Error::new(error.to_string()))?
            .len();
        let (data_start, data_bytes) = data_range.unwrap_or((0, size));
        let data_end = data_start
            .checked_add(data_bytes)
            .ok_or_else(|| Error::new("disc image byte range overflow"))?;
        if data_end > size || !data_bytes.is_multiple_of(layout.sector_size as u64) {
            return Err(Error::new(format!(
                "{} has a truncated or misaligned {}-byte sector range",
                image_path.display(),
                layout.sector_size
            )));
        }
        Ok(Self {
            image_path: image_path.to_path_buf(),
            layout,
            sector_count: data_bytes / layout.sector_size as u64,
            data_start,
            file,
        })
    }

    fn infer_layout(path: &Path) -> Result<Layout, Error> {
        let mut file = File::open(path).map_err(|error| {
            Error::new(format!("cannot open image {}: {error}", path.display()))
        })?;
        let size = file
            .metadata()
            .map_err(|error| Error::new(error.to_string()))?
            .len();
        let mut head = [0u8; 16];
        let read = file
            .read(&mut head)
            .map_err(|error| Error::new(error.to_string()))?;
        if read >= head.len() && head[..12] == SYNC {
            let modes = match head[15] {
                2 => ["MODE2/2352", "MODE2/2448"],
                1 => ["MODE1/2352", "MODE1/2448"],
                mode => {
                    return Err(Error::new(format!("unsupported raw sector mode {mode}")));
                }
            };
            for mode in modes {
                let layout = layout_for_mode(mode)?;
                if size.is_multiple_of(layout.sector_size as u64)
                    && Self::has_iso9660_signature(&mut file, size, layout)?
                {
                    return Ok(layout);
                }
            }
            for mode in modes {
                let layout = layout_for_mode(mode)?;
                if size.is_multiple_of(layout.sector_size as u64) {
                    return Ok(layout);
                }
            }
            return layout_for_mode(modes[0]);
        }

        let cooked = layout_for_mode("MODE1/2048")?;
        if size.is_multiple_of(cooked.sector_size as u64)
            && Self::has_iso9660_signature(&mut file, size, cooked)?
        {
            return Ok(cooked);
        }
        for mode in ["MODE2/2336", "MODE1/2336"] {
            let layout = layout_for_mode(mode)?;
            if size.is_multiple_of(layout.sector_size as u64)
                && Self::has_iso9660_signature(&mut file, size, layout)?
            {
                return Ok(layout);
            }
        }
        if size.is_multiple_of(RAW_SECTOR as u64) {
            for mode in ["MODE1/2352", "MODE2/2352"] {
                let layout = layout_for_mode(mode)?;
                if Self::has_iso9660_signature(&mut file, size, layout)? {
                    return Ok(layout);
                }
            }
            return layout_for_mode("MODE2/2352");
        }
        if size.is_multiple_of(ISO_SECTOR as u64) {
            return Ok(cooked);
        }
        if size.is_multiple_of(2336) {
            return layout_for_mode("MODE2/2336");
        }
        Err(Error::new(format!(
            "{} is not a recognizable disc image",
            path.display()
        )))
    }

    fn has_iso9660_signature(file: &mut File, size: u64, layout: Layout) -> Result<bool, Error> {
        let offset = 16 * layout.sector_size as u64 + layout.data_offset as u64 + 1;
        if offset.checked_add(5).is_none_or(|end| end > size) {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| {
                let mut signature = [0u8; 5];
                file.read_exact(&mut signature)?;
                Ok(signature == *b"CD001")
            })
            .map_err(|error| Error::new(format!("cannot probe ISO signature: {error}")))
    }

    /// Read the user data of one logical sector.
    ///
    /// # Errors
    ///
    /// Returns `UnexpectedEof` for an LBA outside the data track, or an I/O error.
    pub fn read_lba(&mut self, lba: u64) -> io::Result<[u8; ISO_SECTOR]> {
        if lba >= self.sector_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("LBA {lba} is outside {}", self.image_path.display()),
            ));
        }
        let offset = self.byte_offset(lba, "LBA")?;
        let mut sector = [0u8; ISO_SECTOR];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut sector)?;
        Ok(sector)
    }

    /// Feed `size` bytes of user data starting at `lba` to `feed`, one sector
    /// at a time, returning the number of bytes streamed.
    ///
    /// # Errors
    ///
    /// Returns `UnexpectedEof` if the extent leaves the data track, or an I/O error.
    pub fn stream_extent<F>(&mut self, lba: u64, size: u64, mut feed: F) -> io::Result<u64>
    where
        F: FnMut(&[u8]),
    {
        let sectors = size.div_ceil(ISO_SECTOR as u64);
        if lba
            .checked_add(sectors)
            .is_none_or(|end| end > self.sector_count)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "extent exceeds the selected data track",
            ));
        }
        let mut sector = [0u8; ISO_SECTOR];
        let mut remaining = size;
        let mut current_lba = lba;
        let mut streamed = 0;
        while remaining > 0 {
            let offset = self.byte_offset(current_lba, "extent")?;
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.read_exact(&mut sector)?;
            let take = remaining.min(ISO_SECTOR as u64) as usize;
            feed(&sector[..take]);
            remaining -= take as u64;
            streamed += take as u64;
            current_lba += 1;
        }
        Ok(streamed)
    }

    fn byte_offset(&self, lba: u64, context: &str) -> io::Result<u64> {
        self.data_start
            .checked_add(
                lba.checked_mul(self.layout.sector_size as u64)
                    .ok_or_else(|| io::Error::other(format!("{context} byte offset overflow")))?,
            )
            .and_then(|offset| offset.checked_add(self.layout.data_offset as u64))
            .ok_or_else(|| io::Error::other(format!("{context} byte offset overflow")))
    }
}
