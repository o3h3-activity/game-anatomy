use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_SFO_BYTES: u64 = 64 * 1024 * 1024;
const SFO_HEADER_SIZE: usize = 20;
const SFO_ENTRY_SIZE: usize = 16;
const SFO_STRING_FORMAT: u16 = 0x0204;
const PBP_HEADER_SIZE: usize = 40;

/// Returns the final boot filename component without interpreting it.
pub(crate) fn console_serial(name: &str) -> Option<String> {
    name.rsplit(|character| character == '/' || character == '\\')
        .next()
        .map(str::to_owned)
}

/// Extracts the raw `DISC_ID` string from a PARAM.SFO payload.
pub(crate) fn sfo_serial(data: &[u8]) -> Option<String> {
    if data.get(..4)? != b"\0PSF" {
        return None;
    }

    let key_table_offset = read_u32(data, 8)? as usize;
    let value_table_offset = read_u32(data, 12)? as usize;
    let entry_count = read_u32(data, 16)? as usize;
    let entries_end = SFO_HEADER_SIZE.checked_add(entry_count.checked_mul(SFO_ENTRY_SIZE)?)?;

    if entries_end > data.len()
        || key_table_offset < entries_end
        || value_table_offset < key_table_offset
        || value_table_offset > data.len()
    {
        return None;
    }

    for entry_index in 0..entry_count {
        let entry_offset = SFO_HEADER_SIZE.checked_add(entry_index.checked_mul(SFO_ENTRY_SIZE)?)?;
        let key_offset = read_u16(data, entry_offset)? as usize;
        let value_format = read_u16(data, entry_offset.checked_add(2)?)?;
        let value_length = read_u32(data, entry_offset.checked_add(4)?)? as usize;
        let value_max_length = read_u32(data, entry_offset.checked_add(8)?)? as usize;
        let value_offset = read_u32(data, entry_offset.checked_add(12)?)? as usize;

        let key_start = key_table_offset.checked_add(key_offset)?;
        let key = nul_terminated(data.get(key_start..value_table_offset)?)?;
        if key != b"DISC_ID" {
            continue;
        }

        if value_format != SFO_STRING_FORMAT || value_length == 0 || value_length > value_max_length
        {
            return None;
        }

        let value_start = value_table_offset.checked_add(value_offset)?;
        let value_max_end = value_start.checked_add(value_max_length)?;
        let value_end = value_start.checked_add(value_length)?;
        if value_max_end > data.len() || value_end > value_max_end {
            return None;
        }

        let value = data.get(value_start..value_end)?;
        let value = std::str::from_utf8(value.strip_suffix(&[0])?).ok()?;
        return Some(value.to_owned());
    }

    None
}

/// Reads the PARAM.SFO section of a PBP without loading the rest of the
/// container.
pub(crate) fn pbp_serial(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let file_length = file.metadata().ok()?.len();
    if file_length < PBP_HEADER_SIZE as u64 {
        return None;
    }

    let mut header = [0; PBP_HEADER_SIZE];
    file.read_exact(&mut header).ok()?;
    if header.get(..4)? != b"\0PBP" {
        return None;
    }

    let mut offsets = [0_u64; 8];
    for (index, offset) in offsets.iter_mut().enumerate() {
        *offset = read_u32(&header, 8 + index * 4)? as u64;
    }

    if offsets[0] < PBP_HEADER_SIZE as u64
        || offsets.iter().any(|&offset| offset > file_length)
        || offsets.windows(2).any(|pair| pair[0] > pair[1])
    {
        return None;
    }

    let param_length = offsets[1].checked_sub(offsets[0])?;
    if param_length > MAX_SFO_BYTES {
        return None;
    }

    let mut param = vec![0; param_length as usize];
    file.seek(SeekFrom::Start(offsets[0])).ok()?;
    file.read_exact(&mut param).ok()?;
    sfo_serial(&param)
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn nul_terminated(data: &[u8]) -> Option<&[u8]> {
    let end = data.iter().position(|&byte| byte == 0)?;
    Some(&data[..end])
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{PBP_HEADER_SIZE, console_serial, pbp_serial, sfo_serial};

    static TEMP_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn console_serial_returns_the_raw_final_path_component() {
        assert_eq!(
            console_serial(r"bOoT\sLuS_000.00"),
            Some("sLuS_000.00".to_owned())
        );
        assert_eq!(console_serial("boot/PSX.EXE"), Some("PSX.EXE".to_owned()));
    }

    #[test]
    fn sfo_serial_preserves_arbitrary_disc_id_text() {
        assert_eq!(
            sfo_serial(&sfo_with_disc_id("arbitrary disc id!")),
            Some("arbitrary disc id!".to_owned())
        );
    }

    #[test]
    fn sfo_serial_rejects_bad_offsets_values_and_types() {
        let valid = sfo_with_disc_id("SLUS12345");

        let mut invalid_offset = valid.clone();
        invalid_offset[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(sfo_serial(&invalid_offset), None);

        let mut invalid_type = valid.clone();
        invalid_type[22..24].copy_from_slice(&0x0404_u16.to_le_bytes());
        assert_eq!(sfo_serial(&invalid_type), None);

        let mut unterminated_value = valid.clone();
        *unterminated_value.last_mut().unwrap() = b'X';
        assert_eq!(sfo_serial(&unterminated_value), None);

        let mut truncated_value = valid;
        truncated_value.pop();
        assert_eq!(sfo_serial(&truncated_value), None);
    }

    #[test]
    fn pbp_serial_reads_only_the_param_sfo_section() {
        let sfo = sfo_with_disc_id("raw/PBP id");
        let path = temporary_path("valid");
        std::fs::write(&path, pbp_with_offsets(&sfo, None)).unwrap();

        let serial = pbp_serial(&path);
        std::fs::remove_file(path).unwrap();

        assert_eq!(serial, Some("raw/PBP id".to_owned()));
    }

    #[test]
    fn pbp_serial_rejects_nonmonotonic_section_offsets() {
        let sfo = sfo_with_disc_id("SLUS12345");
        let path = temporary_path("malformed");
        std::fs::write(
            &path,
            pbp_with_offsets(&sfo, Some([40, 39, 40, 40, 40, 40, 40, 40])),
        )
        .unwrap();

        let serial = pbp_serial(&path);
        std::fs::remove_file(path).unwrap();

        assert_eq!(serial, None);
    }

    fn sfo_with_disc_id(disc_id: &str) -> Vec<u8> {
        let key_table_offset = 36_u32;
        let value_table_offset = 44_u32;
        let value_length = disc_id.len() as u32 + 1;
        let mut sfo = Vec::new();
        sfo.extend_from_slice(b"\0PSF");
        sfo.extend_from_slice(&0x0000_0101_u32.to_le_bytes());
        sfo.extend_from_slice(&key_table_offset.to_le_bytes());
        sfo.extend_from_slice(&value_table_offset.to_le_bytes());
        sfo.extend_from_slice(&1_u32.to_le_bytes());
        sfo.extend_from_slice(&0_u16.to_le_bytes());
        sfo.extend_from_slice(&0x0204_u16.to_le_bytes());
        sfo.extend_from_slice(&value_length.to_le_bytes());
        sfo.extend_from_slice(&value_length.to_le_bytes());
        sfo.extend_from_slice(&0_u32.to_le_bytes());
        sfo.extend_from_slice(b"DISC_ID\0");
        sfo.extend_from_slice(disc_id.as_bytes());
        sfo.push(0);
        sfo
    }

    fn pbp_with_offsets(sfo: &[u8], offsets: Option<[u32; 8]>) -> Vec<u8> {
        let sfo_end = PBP_HEADER_SIZE as u32 + sfo.len() as u32;
        let offsets = offsets.unwrap_or([
            sfo_end - sfo.len() as u32,
            sfo_end,
            sfo_end,
            sfo_end,
            sfo_end,
            sfo_end,
            sfo_end,
            sfo_end,
        ]);
        let mut pbp = Vec::with_capacity(PBP_HEADER_SIZE + sfo.len());
        pbp.extend_from_slice(b"\0PBP");
        pbp.extend_from_slice(&0x0001_0000_u32.to_le_bytes());
        for offset in offsets {
            pbp.extend_from_slice(&offset.to_le_bytes());
        }
        pbp.extend_from_slice(sfo);
        pbp
    }

    fn temporary_path(label: &str) -> std::path::PathBuf {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "game-anatomy-serial-{label}-{}-{counter}.pbp",
            std::process::id()
        ))
    }
}
