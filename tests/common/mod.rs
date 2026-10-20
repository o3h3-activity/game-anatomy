use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SECTOR: u64 = 2048;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct IsoFile {
    path: String,
    data: Vec<u8>,
    declared_size: u32,
}

impl IsoFile {
    pub(crate) fn new(path: &str, data: Vec<u8>) -> Self {
        let declared_size = data
            .len()
            .try_into()
            .expect("test fixture file fits in u32");
        Self {
            path: path.into(),
            data,
            declared_size,
        }
    }

    pub(crate) fn declared(path: &str, data: Vec<u8>, declared_size: u32) -> Self {
        Self {
            path: path.into(),
            data,
            declared_size,
        }
    }
}

pub(crate) struct TempFixture {
    path: PathBuf,
    file_lbas: BTreeMap<String, u32>,
}

impl TempFixture {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn truncate_file(&self, path: &str, available_bytes: u64) {
        let lba = self.file_lbas[path];
        OpenOptions::new()
            .write(true)
            .open(&self.path)
            .expect("open fixture for truncation")
            .set_len(u64::from(lba) * SECTOR + available_bytes)
            .expect("truncate fixture extent");
    }

    pub(crate) fn overwrite(&self, offset: u64, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&self.path)
            .expect("open fixture for overwrite");
        file.seek(SeekFrom::Start(offset))
            .expect("seek fixture overwrite");
        file.write_all(bytes).expect("overwrite fixture bytes");
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn write_bytes(extension: &str, data: &[u8]) -> TempFixture {
    let path = fixture_path(extension);
    std::fs::write(&path, data).expect("write raw fixture");
    TempFixture {
        path,
        file_lbas: BTreeMap::new(),
    }
}

pub(crate) fn write_iso(files: Vec<IsoFile>) -> TempFixture {
    let mut directories = BTreeSet::from([String::new()]);
    for file in &files {
        let components = file.path.split('/').collect::<Vec<_>>();
        let mut current = String::new();
        for component in &components[..components.len().saturating_sub(1)] {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(component);
            directories.insert(current.clone());
        }
    }

    let mut directory_lbas = BTreeMap::new();
    for (index, directory) in directories.iter().enumerate() {
        directory_lbas.insert(directory.clone(), 20 + index as u32);
    }
    let mut next_lba = 20 + directory_lbas.len() as u32;
    let mut file_lbas = BTreeMap::new();
    for file in &files {
        file_lbas.insert(file.path.clone(), next_lba);
        let reserved = u64::from(file.declared_size)
            .max(file.data.len() as u64)
            .div_ceil(SECTOR)
            .max(1);
        next_lba += reserved as u32;
    }
    let total_sectors = next_lba.max(24);

    let path = fixture_path("iso");
    let mut output = File::create(&path).expect("create ISO fixture");
    output
        .set_len(u64::from(total_sectors) * SECTOR)
        .expect("size ISO fixture");

    let root_lba = directory_lbas[""];
    let root_record = directory_record(&[0], root_lba, SECTOR as u32, 0x02);
    let mut pvd = [0u8; SECTOR as usize];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    write_padded(&mut pvd[8..40], b"DISC_BLUEPRINT_TEST");
    write_padded(&mut pvd[40..72], b"TEST_VOLUME");
    both_u32(&mut pvd[80..88], total_sectors);
    both_u16(&mut pvd[120..124], 1);
    both_u16(&mut pvd[124..128], 1);
    both_u16(&mut pvd[128..132], SECTOR as u16);
    pvd[156..156 + root_record.len()].copy_from_slice(&root_record);
    for offset in [813, 830, 847, 864] {
        pvd[offset..offset + 17].copy_from_slice(b"2026010100000000\0");
    }
    pvd[881] = 1;
    write_sector(&mut output, 16, &pvd);

    let mut terminator = [0u8; SECTOR as usize];
    terminator[0] = 255;
    terminator[1..6].copy_from_slice(b"CD001");
    terminator[6] = 1;
    write_sector(&mut output, 17, &terminator);

    for directory in &directories {
        let directory_lba = directory_lbas[directory];
        let parent = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        let parent_lba = directory_lbas[parent];
        let mut sector = [0u8; SECTOR as usize];
        let mut cursor = 0;
        append_record(
            &mut sector,
            &mut cursor,
            &directory_record(&[0], directory_lba, SECTOR as u32, 0x02),
        );
        append_record(
            &mut sector,
            &mut cursor,
            &directory_record(&[1], parent_lba, SECTOR as u32, 0x02),
        );

        for child in directories
            .iter()
            .filter(|candidate| direct_parent(candidate) == directory && *candidate != directory)
        {
            let name = child.rsplit('/').next().expect("directory name");
            append_record(
                &mut sector,
                &mut cursor,
                &directory_record(name.as_bytes(), directory_lbas[child], SECTOR as u32, 0x02),
            );
        }
        for file in files
            .iter()
            .filter(|file| direct_parent(&file.path) == directory)
        {
            let name = file.path.rsplit('/').next().expect("file name");
            let versioned = format!("{name};1");
            append_record(
                &mut sector,
                &mut cursor,
                &directory_record(
                    versioned.as_bytes(),
                    file_lbas[&file.path],
                    file.declared_size,
                    0,
                ),
            );
        }
        write_sector(&mut output, directory_lba, &sector);
    }

    for file in &files {
        output
            .seek(SeekFrom::Start(u64::from(file_lbas[&file.path]) * SECTOR))
            .expect("seek fixture file extent");
        output
            .write_all(&file.data)
            .expect("write fixture file extent");
    }
    output.flush().expect("flush ISO fixture");

    TempFixture { path, file_lbas }
}

pub(crate) fn write_mode2_2448_iso(files: Vec<IsoFile>) -> TempFixture {
    const RAW_SECTOR: usize = 2448;
    const DATA_OFFSET: usize = 24;
    const SYNC: [u8; 12] = [
        0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
    ];

    let iso = write_iso(files);
    let cooked = std::fs::read(iso.path()).expect("read cooked ISO fixture");
    let mut raw = vec![0u8; cooked.len() / SECTOR as usize * RAW_SECTOR];
    for (index, payload) in cooked
        .as_chunks::<{ SECTOR as usize }>()
        .0
        .iter()
        .enumerate()
    {
        let start = index * RAW_SECTOR;
        raw[start..start + SYNC.len()].copy_from_slice(&SYNC);
        raw[start + 15] = 2;
        raw[start + DATA_OFFSET..start + DATA_OFFSET + payload.len()].copy_from_slice(payload);
    }
    write_bytes("mdf", &raw)
}

fn fixture_path(extension: &str) -> PathBuf {
    let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "disc-blueprint-{}-{serial}.{extension}",
        std::process::id()
    ))
}

fn direct_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn append_record(sector: &mut [u8], cursor: &mut usize, record: &[u8]) {
    assert!(
        *cursor + record.len() <= sector.len(),
        "fixture directory fits one sector"
    );
    sector[*cursor..*cursor + record.len()].copy_from_slice(record);
    *cursor += record.len();
}

fn directory_record(name: &[u8], lba: u32, size: u32, flags: u8) -> Vec<u8> {
    let unpadded = 33 + name.len();
    let length = if unpadded.is_multiple_of(2) {
        unpadded
    } else {
        unpadded + 1
    };
    let mut record = vec![0u8; length];
    record[0] = length as u8;
    record[2..6].copy_from_slice(&lba.to_le_bytes());
    record[6..10].copy_from_slice(&lba.to_be_bytes());
    record[10..14].copy_from_slice(&size.to_le_bytes());
    record[14..18].copy_from_slice(&size.to_be_bytes());
    record[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    record[25] = flags;
    record[28..30].copy_from_slice(&1u16.to_le_bytes());
    record[30..32].copy_from_slice(&1u16.to_be_bytes());
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
    record
}

fn write_sector(file: &mut File, lba: u32, sector: &[u8; SECTOR as usize]) {
    file.seek(SeekFrom::Start(u64::from(lba) * SECTOR))
        .expect("seek ISO sector");
    file.write_all(sector).expect("write ISO sector");
}

fn write_padded(output: &mut [u8], value: &[u8]) {
    output.fill(b' ');
    output[..value.len()].copy_from_slice(value);
}

fn both_u16(output: &mut [u8], value: u16) {
    output[..2].copy_from_slice(&value.to_le_bytes());
    output[2..4].copy_from_slice(&value.to_be_bytes());
}

fn both_u32(output: &mut [u8], value: u32) {
    output[..4].copy_from_slice(&value.to_le_bytes());
    output[4..8].copy_from_slice(&value.to_be_bytes());
}
