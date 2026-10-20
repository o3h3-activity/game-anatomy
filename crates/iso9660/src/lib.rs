//! Bounded ISO 9660 file lookup over any 2048-byte sector source.
//!
//! Directory reads are cached per request and share one metadata budget
//! (depth, cumulative bytes, record count, and non-overlapping sector ranges),
//! so hostile images cannot force unbounded work. Multi-extent and
//! interleaved files are supported.

use std::collections::{BTreeMap, HashMap};
use std::io;

/// Logical ISO 9660 sector size in bytes.
pub const ISO_SECTOR: usize = 2048;

/// Random-access storage addressed by 2048-byte logical sectors.
pub trait SectorSource {
    /// Read one logical sector.
    ///
    /// # Errors
    ///
    /// Returns an error if the sector is out of range or unreadable.
    fn read_lba(&mut self, lba: u64) -> io::Result<[u8; ISO_SECTOR]>;

    /// Feed `size` bytes of consecutive sectors starting at `lba` to `feed`,
    /// returning the number of bytes streamed.
    ///
    /// # Errors
    ///
    /// Returns an error if any sector of the extent is out of range or unreadable.
    fn stream_extent<F>(&mut self, lba: u64, size: u64, feed: F) -> io::Result<u64>
    where
        F: FnMut(&[u8]);
}

const MAX_BUFFERED_EXTENT: u64 = 64 << 20;
const MAX_DIRECTORY_DEPTH: usize = 8;
const MAX_DIRECTORY_BYTES: u64 = 128 << 20;
const MAX_PARSED_DIRECTORY_RECORDS: usize = 65_536;

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Read the first primary volume descriptor's root directory. The descriptor
/// set is consumed through its terminator (or first non-descriptor sector) so
/// malformed primary descriptors are not hidden by an earlier valid one.
///
/// Returns `Ok(None)` when descriptors exist but none is primary.
///
/// # Errors
///
/// Returns an error when sector 16 holds no descriptor, a descriptor is
/// unreadable, or the primary root directory record is malformed.
pub fn read_root<S: SectorSource>(disc: &mut S) -> Result<Option<DirEntry>, String> {
    let mut root = None;
    let mut found_descriptor = false;
    for lba in 16..80 {
        let sector = disc
            .read_lba(lba)
            .map_err(|error| format!("read VD @{lba}: {error}"))?;
        if &sector[1..6] != b"CD001" {
            break;
        }
        found_descriptor = true;
        match sector[0] {
            1 => {
                let parsed = parse_primary_root(&sector)?;
                if root.is_none() {
                    root = Some(parsed);
                }
            }
            255 => break,
            _ => {}
        }
    }
    if !found_descriptor {
        return Err("no ISO 9660 volume descriptors found at sector 16".into());
    }
    Ok(root)
}

fn parse_primary_root(sector: &[u8; ISO_SECTOR]) -> Result<DirEntry, String> {
    let root_len = usize::from(sector[156]);
    if root_len != 34 {
        return Err(format!(
            "PVD root directory record has length {root_len}, expected 34"
        ));
    }
    parse_dir_record(&sector[156..156 + root_len])
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct FileExtent {
    lba: u32,
    size: u32,
    file_unit_size: u8,
    interleave_gap: u8,
}

/// A directory record, with multi-extent files merged into one entry.
#[derive(Clone)]
pub struct DirEntry {
    pub name: String,
    pub size: u64,
    extents: Vec<FileExtent>,
    flags: u8,
}

impl DirEntry {
    fn is_dir(&self) -> bool {
        self.flags & 0x02 != 0
    }

    fn is_associated(&self) -> bool {
        self.flags & 0x04 != 0
    }

    fn is_real_file(&self) -> bool {
        self.name != "." && self.name != ".."
    }

    /// Stream the first `size` bytes of this entry's data to `feed`.
    ///
    /// # Errors
    ///
    /// Returns an error if `size` exceeds the declared size, the extents are
    /// malformed, or the underlying sectors are unavailable.
    pub fn stream<S, F>(&self, disc: &mut S, size: u64, mut feed: F) -> io::Result<u64>
    where
        S: SectorSource,
        F: FnMut(&[u8]),
    {
        if size > self.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "{} declares {} bytes but {size} bytes were requested",
                    self.name, self.size
                ),
            ));
        }
        let mut remaining = size;
        let mut streamed = 0;
        for extent in &self.extents {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(u64::from(extent.size));
            if extent.file_unit_size == 0 {
                if extent.interleave_gap != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} has an interleave gap without a file unit", self.name),
                    ));
                }
                disc.stream_extent(u64::from(extent.lba), take, |bytes| feed(bytes))?;
            } else {
                let unit_bytes = u64::from(extent.file_unit_size) * ISO_SECTOR as u64;
                let mut extent_remaining = take;
                let mut lba = u64::from(extent.lba);
                while extent_remaining > 0 {
                    let unit_take = extent_remaining.min(unit_bytes);
                    disc.stream_extent(lba, unit_take, |bytes| feed(bytes))?;
                    extent_remaining -= unit_take;
                    if extent_remaining != 0 {
                        lba = lba
                            .checked_add(unit_take.div_ceil(ISO_SECTOR as u64))
                            .and_then(|next| next.checked_add(u64::from(extent.interleave_gap)))
                            .ok_or_else(|| io::Error::other("interleaved extent overflow"))?;
                    }
                }
            }
            remaining -= take;
            streamed += take;
        }
        if remaining != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} is missing {remaining} declared bytes", self.name),
            ));
        }
        Ok(streamed)
    }

    /// Buffer the first `size` bytes of this entry's data.
    ///
    /// # Errors
    ///
    /// As [`DirEntry::stream`]; also refuses buffers larger than 64 MiB.
    pub fn read<S: SectorSource>(&self, disc: &mut S, size: u64) -> io::Result<Vec<u8>> {
        if size > MAX_BUFFERED_EXTENT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("refusing to buffer a {size}-byte ISO extent"),
            ));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        self.stream(disc, size, |chunk| bytes.extend_from_slice(chunk))?;
        Ok(bytes)
    }
}

fn parse_dir_record(record: &[u8]) -> Result<DirEntry, String> {
    if record.len() < 33 {
        return Err(format!(
            "directory record too short ({} bytes)",
            record.len()
        ));
    }
    let name_len = usize::from(record[32]);
    if 33 + name_len > record.len() {
        return Err("directory record name overruns record".into());
    }
    let name = match name_len {
        0 => String::new(),
        1 if record[33] == 0x00 => String::from("."),
        1 if record[33] == 0x01 => String::from(".."),
        _ => String::from_utf8_lossy(&record[33..33 + name_len]).to_string(),
    };
    let extent_lba = le32(&record[2..6]);
    let lba = extent_lba
        .checked_add(u32::from(record[1]))
        .ok_or_else(|| format!("extended-attribute LBA overflow for {name}"))?;
    let extent_size = le32(&record[10..14]);
    Ok(DirEntry {
        name,
        size: u64::from(extent_size),
        extents: vec![FileExtent {
            lba,
            size: extent_size,
            file_unit_size: record[26],
            interleave_gap: record[27],
        }],
        flags: record[25],
    })
}

fn read_dir_extent<S: SectorSource>(
    disc: &mut S,
    directory: &DirEntry,
    parsed_records: &mut usize,
) -> Result<Vec<DirEntry>, String> {
    let bytes = directory
        .read(disc, directory.size)
        .map_err(|error| error.to_string())?;
    let mut entries: Vec<DirEntry> = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let len = usize::from(bytes[position]);
        if len == 0 {
            position = (position / ISO_SECTOR + 1) * ISO_SECTOR;
            continue;
        }
        if position % ISO_SECTOR + len > ISO_SECTOR || position + len > bytes.len() {
            return Err(format!(
                "malformed directory record @offset {position} (len {len})"
            ));
        }
        if *parsed_records >= MAX_PARSED_DIRECTORY_RECORDS {
            return Err(format!(
                "ISO directory records exceed {MAX_PARSED_DIRECTORY_RECORDS}"
            ));
        }
        *parsed_records += 1;
        let entry = parse_dir_record(&bytes[position..position + len])?;
        if let Some(last) = entries.last_mut()
            && last.flags & 0x80 != 0
        {
            if last.name != entry.name {
                return Err(format!(
                    "multi-extent file {} is missing its next extent",
                    last.name
                ));
            }
            last.size = last
                .size
                .checked_add(entry.size)
                .ok_or_else(|| format!("multi-extent size overflow for {}", entry.name))?;
            last.extents.extend(entry.extents);
            last.flags = entry.flags;
            position += len;
            continue;
        }
        entries.push(entry);
        position += len;
    }
    if entries.last().is_some_and(|entry| entry.flags & 0x80 != 0) {
        return Err(format!(
            "multi-extent file {} ends without a final extent",
            entries.last().expect("last entry exists").name
        ));
    }
    Ok(entries)
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct DirectoryKey {
    size: u64,
    extents: Vec<FileExtent>,
}

impl From<&DirEntry> for DirectoryKey {
    fn from(entry: &DirEntry) -> Self {
        Self {
            size: entry.size,
            extents: entry.extents.clone(),
        }
    }
}

#[derive(Default)]
struct DirectoryBudget {
    directory_bytes: u64,
    parsed_records: usize,
    physical_ranges: BTreeMap<u64, u64>,
}

impl DirectoryBudget {
    fn consume_directory(&mut self, entry: &DirEntry) -> Result<(), String> {
        self.directory_bytes = self
            .directory_bytes
            .checked_add(entry.size)
            .ok_or_else(|| "ISO cumulative directory size overflows".to_string())?;
        if self.directory_bytes > MAX_DIRECTORY_BYTES {
            return Err(format!(
                "ISO cumulative directory bytes exceed {MAX_DIRECTORY_BYTES}"
            ));
        }
        for (start, end) in directory_physical_ranges(entry)? {
            if self
                .physical_ranges
                .range(..=start)
                .next_back()
                .is_some_and(|(_, previous_end)| *previous_end > start)
                || self
                    .physical_ranges
                    .range(start..)
                    .next()
                    .is_some_and(|(next_start, _)| *next_start < end)
            {
                return Err(format!(
                    "directory {} overlaps a previously declared directory sector range {start}..{end}",
                    entry.name
                ));
            }
            self.physical_ranges.insert(start, end);
        }
        Ok(())
    }
}

fn directory_physical_ranges(entry: &DirEntry) -> Result<Vec<(u64, u64)>, String> {
    let mut remaining = entry.size;
    let mut ranges = Vec::new();
    for extent in &entry.extents {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(u64::from(extent.size));
        if extent.file_unit_size == 0 {
            if extent.interleave_gap != 0 {
                return Err(format!(
                    "{} has an interleave gap without a file unit",
                    entry.name
                ));
            }
            push_sector_range(
                &mut ranges,
                u64::from(extent.lba),
                take.div_ceil(ISO_SECTOR as u64),
            )?;
        } else {
            let unit_bytes = u64::from(extent.file_unit_size) * ISO_SECTOR as u64;
            let mut extent_remaining = take;
            let mut lba = u64::from(extent.lba);
            while extent_remaining > 0 {
                let unit_take = extent_remaining.min(unit_bytes);
                push_sector_range(&mut ranges, lba, unit_take.div_ceil(ISO_SECTOR as u64))?;
                extent_remaining -= unit_take;
                if extent_remaining != 0 {
                    lba = lba
                        .checked_add(unit_take.div_ceil(ISO_SECTOR as u64))
                        .and_then(|next| next.checked_add(u64::from(extent.interleave_gap)))
                        .ok_or_else(|| "interleaved directory extent overflow".to_string())?;
                }
            }
        }
        remaining -= take;
    }
    if remaining != 0 {
        return Err(format!(
            "{} is missing {remaining} declared directory bytes",
            entry.name
        ));
    }
    Ok(ranges)
}

fn push_sector_range(ranges: &mut Vec<(u64, u64)>, start: u64, sectors: u64) -> Result<(), String> {
    if sectors != 0 {
        let end = start
            .checked_add(sectors)
            .ok_or_else(|| "directory sector range overflows".to_string())?;
        ranges.push((start, end));
    }
    Ok(())
}

/// A request-scoped, cache-backed ISO lookup with one shared metadata budget.
pub struct TargetedIso<'a, S> {
    disc: &'a mut S,
    root: DirEntry,
    directories: HashMap<DirectoryKey, Result<Vec<DirEntry>, String>>,
    budget: DirectoryBudget,
}

impl<'a, S: SectorSource> TargetedIso<'a, S> {
    pub fn new(disc: &'a mut S, root: DirEntry) -> Self {
        Self {
            disc,
            root,
            directories: HashMap::new(),
            budget: DirectoryBudget::default(),
        }
    }

    /// Resolve a `/`- or `\`-separated path case-insensitively; a component
    /// without `;` ignores the entry's version suffix. Directories and
    /// associated files never match the final component.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed directories or an exceeded metadata budget.
    pub fn find_file(&mut self, wanted: &str) -> Result<Option<DirEntry>, String> {
        let components: Vec<_> = wanted
            .trim_start_matches(['/', '\\'])
            .split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .collect();
        if components.is_empty() {
            return Ok(None);
        }

        let mut current = self.root.clone();
        for (index, component) in components.iter().enumerate() {
            if index >= MAX_DIRECTORY_DEPTH {
                return Err(format!(
                    "ISO directory depth exceeds {MAX_DIRECTORY_DEPTH} levels while resolving {wanted}"
                ));
            }
            let final_component = index + 1 == components.len();
            let candidate = self
                .read_directory(&current)?
                .iter()
                .find(|entry| {
                    entry.is_real_file()
                        && !entry.is_associated()
                        && component_matches(&entry.name, component)
                        && (final_component || entry.is_dir())
                })
                .cloned();
            let Some(candidate) = candidate else {
                return Ok(None);
            };
            if final_component {
                return Ok((!candidate.is_dir()).then_some(candidate));
            }
            current = candidate;
        }
        Ok(None)
    }

    /// Buffer the first `size` bytes of `entry`; see [`DirEntry::read`].
    ///
    /// # Errors
    ///
    /// As [`DirEntry::read`].
    pub fn read_entry(&mut self, entry: &DirEntry, size: u64) -> io::Result<Vec<u8>> {
        entry.read(self.disc, size)
    }

    /// Stream the first `size` bytes of `entry`; see [`DirEntry::stream`].
    ///
    /// # Errors
    ///
    /// As [`DirEntry::stream`].
    pub fn stream_entry<F>(
        &mut self,
        entry: &DirEntry,
        size: u64,
        feed: F,
    ) -> io::Result<u64>
    where
        F: FnMut(&[u8]),
    {
        entry.stream(self.disc, size, feed)
    }

    fn read_directory(&mut self, entry: &DirEntry) -> Result<&[DirEntry], String> {
        let key = DirectoryKey::from(entry);
        if !self.directories.contains_key(&key) {
            let entries = self
                .budget
                .consume_directory(entry)
                .and_then(|()| read_dir_extent(self.disc, entry, &mut self.budget.parsed_records));
            self.directories.insert(key.clone(), entries);
        }
        match self
            .directories
            .get(&key)
            .expect("directory result was cached")
        {
            Ok(entries) => Ok(entries),
            Err(error) => Err(error.clone()),
        }
    }
}

fn component_matches(entry_name: &str, wanted: &str) -> bool {
    let entry_name = if wanted.contains(';') {
        entry_name
    } else {
        entry_name.split(';').next().unwrap_or(entry_name)
    };
    entry_name.eq_ignore_ascii_case(wanted)
}
