mod common;

use common::{IsoFile, write_bytes, write_iso, write_mode2_2448_iso};
use game_anatomy::{GameInfo, game_info, generate_hash};

fn ps1_executable() -> Vec<u8> {
    let mut executable = vec![0; 2048];
    executable[..8].copy_from_slice(b"PS-X EXE");
    executable
}

fn ps2_files() -> Vec<IsoFile> {
    let mut executable = vec![b'2'; 2048];
    executable[..4].copy_from_slice(b"\x7fELF");
    vec![
        IsoFile::new("SYSTEM.CNF", b"BOOT2 = cdrom0:\\SLUS_999.99;1\n".to_vec()),
        IsoFile::new("SLUS_999.99", executable),
    ]
}

#[test]
fn ps1_matches_pinned_rcheevos_vector() {
    let mut executable = ps1_executable();
    executable[0x20..].fill(b'P');
    let iso = write_iso(vec![
        IsoFile::new("SYSTEM.CNF", b"BOOT = cdrom:\\SLUS_000.00;1\n".to_vec()),
        IsoFile::new("SLUS_000.00", executable),
    ]);
    assert_eq!(
        game_info(iso.path()).unwrap(),
        GameInfo {
            platform: "ps1",
            serial: Some("SLUS_000.00".into()),
            identity_md5: "1d87789d1cb20d79d9ddba593cffd167".into(),
            retroachievements_platform_id: 12,
        }
    );
}

#[test]
fn ps2_matches_pinned_rcheevos_vector() {
    let iso = write_iso(ps2_files());
    assert_eq!(
        game_info(iso.path()).unwrap(),
        GameInfo {
            platform: "ps2",
            serial: Some("SLUS_999.99".into()),
            identity_md5: "9745cd29905510c2ea5f801075753cf8".into(),
            retroachievements_platform_id: 21,
        }
    );
}

#[test]
fn psp_iso_matches_pinned_rcheevos_vector() {
    let mut param = vec![b'S'; 2048];
    param[..4].copy_from_slice(b"PSF\0");
    let mut eboot = vec![b'E'; 2048];
    eboot[..5].copy_from_slice(b"EBOOT");
    let iso = write_iso(vec![
        IsoFile::new("PSP_GAME/PARAM.SFO", param),
        IsoFile::new("PSP_GAME/SYSDIR/EBOOT.BIN", eboot),
    ]);
    assert_eq!(
        game_info(iso.path()).unwrap(),
        GameInfo {
            platform: "psp",
            serial: None,
            identity_md5: "6b0388bbe348d7be48ee81a5737e80ba".into(),
            retroachievements_platform_id: 41,
        }
    );
}

#[test]
fn pbp_uses_magic_not_extension_and_rejects_invalid_magic() {
    let mut bytes = vec![b'B'; 1024];
    bytes[..4].copy_from_slice(b"\0PBP");
    let pbp = write_bytes("dat", &bytes);
    assert_eq!(
        generate_hash(pbp.path()).unwrap(),
        "67c125036bf47ede8ce642c05f304fb0"
    );
    let invalid = write_bytes("pbp", b"NOPE");
    assert!(generate_hash(invalid.path()).is_err());
}

#[test]
fn ps1_header_selects_content_and_boot_token_preserves_case_without_version() {
    let mut executable = ps1_executable();
    // Bytes beyond the header-selected 2048 bytes must not enter the hash.
    executable.extend_from_slice(&[b'X'; 2048]);
    let iso = write_iso(vec![
        IsoFile::new(
            "SYSTEM.CNF",
            b"BOOT = cdrom:\\bOoT\\sLuS_000.00;9\n".to_vec(),
        ),
        IsoFile::new("Boot/Slus_000.00", executable),
    ]);
    assert_eq!(
        generate_hash(iso.path()).unwrap(),
        "863878a97c27a70a6852da6c2bb2970b"
    );
    assert_eq!(
        game_info(iso.path()).unwrap().serial.as_deref(),
        Some("sLuS_000.00")
    );
}

#[test]
fn ps1_falls_back_to_root_executable() {
    let iso = write_iso(vec![IsoFile::new("PSX.EXE", ps1_executable())]);
    assert_eq!(
        generate_hash(iso.path()).unwrap(),
        "501f5882147cac7900455053a479b219"
    );
    assert_eq!(game_info(iso.path()).unwrap().serial, None);
}

#[test]
fn hash_ignores_unused_path_table_but_rejects_truncated_selected_content() {
    let iso = write_iso(ps2_files());
    iso.overwrite(16 * 2048 + 132, &u32::MAX.to_le_bytes());
    iso.overwrite(16 * 2048 + 140, &u32::MAX.to_le_bytes());
    assert_eq!(
        generate_hash(iso.path()).unwrap(),
        "9745cd29905510c2ea5f801075753cf8"
    );
    iso.truncate_file("SLUS_999.99", 2047);
    assert!(generate_hash(iso.path()).is_err());
}

#[test]
fn ps1_rejects_missing_header_selected_bytes() {
    let mut executable = ps1_executable();
    executable[0x1c..0x20].copy_from_slice(&2048u32.to_le_bytes());
    let iso = write_iso(vec![IsoFile::declared("PSX.EXE", executable, 2048)]);
    assert!(generate_hash(iso.path()).is_err());
}

#[test]
fn missing_psp_file_and_ambiguous_console_evidence_emit_no_hash() {
    let incomplete = write_iso(vec![IsoFile::new("PSP_GAME/PARAM.SFO", vec![0; 32])]);
    assert!(generate_hash(incomplete.path()).is_err());
    let mut files = ps2_files();
    files.push(IsoFile::new("PSX.EXE", ps1_executable()));
    let ambiguous = write_iso(files);
    assert!(generate_hash(ambiguous.path()).is_err());
}

#[test]
fn raw_sector_framing_and_cue_offsets_preserve_game_hash() {
    let mdf = write_mode2_2448_iso(ps2_files());
    assert_eq!(
        generate_hash(mdf.path()).unwrap(),
        "9745cd29905510c2ea5f801075753cf8"
    );
    let cooked = write_iso(ps2_files());
    let bytes = std::fs::read(cooked.path()).unwrap();
    for sector_size in [2336, 2352] {
        let offset = if sector_size == 2336 { 8 } else { 24 };
        let mut raw = vec![0; bytes.len() / 2048 * sector_size];
        for (index, payload) in bytes.as_chunks::<2048>().0.iter().enumerate() {
            let sector = &mut raw[index * sector_size..(index + 1) * sector_size];
            if sector_size == 2352 {
                sector[1..11].fill(0xff);
                sector[15] = 2;
            }
            sector[offset..offset + 2048].copy_from_slice(payload);
        }
        let bin = write_bytes("bin", &raw);
        assert_eq!(
            generate_hash(bin.path()).unwrap(),
            "9745cd29905510c2ea5f801075753cf8"
        );
        // INDEX 01 skips a physical-sector prefix in the same file.
        let mut prefixed = vec![0; sector_size * 150];
        prefixed.extend_from_slice(&raw);
        let track = write_bytes("bin", &prefixed);
        let cue = write_bytes(
            "cue",
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE2/{sector_size}\n    INDEX 01 00:02:00\n",
                track.path().display()
            )
            .as_bytes(),
        );
        assert_eq!(
            generate_hash(cue.path()).unwrap(),
            "9745cd29905510c2ea5f801075753cf8"
        );
    }
}

#[test]
fn pbp_caps_content_at_64_mib() {
    let mut bytes = vec![0; 64 * 1024 * 1024 + 2048];
    bytes[..4].copy_from_slice(b"\0PBP");
    bytes[64 * 1024 * 1024..].fill(b'X');
    let pbp = write_bytes("pbp", &bytes);
    assert_eq!(
        generate_hash(pbp.path()).unwrap(),
        "5ae7d49682dfb4b7990d1124b0dadce0"
    );
}

#[test]
fn complete_psp_evidence_wins_even_when_other_validator_cannot_read_configuration() {
    let mut files = vec![
        IsoFile::new("PSP_GAME/PARAM.SFO", b"metadata".to_vec()),
        IsoFile::new("PSP_GAME/SYSDIR/EBOOT.BIN", b"executable".to_vec()),
    ];
    files.extend(ps2_files());
    let iso = write_iso(files);
    assert_eq!(
        generate_hash(iso.path()).unwrap(),
        "43c780f6bcecb4bc8b32bb3b7ad7a5ff"
    );
    iso.truncate_file("SYSTEM.CNF", 0);
    assert_eq!(
        generate_hash(iso.path()).unwrap(),
        "43c780f6bcecb4bc8b32bb3b7ad7a5ff"
    );
}

#[test]
fn unresolved_boot_target_does_not_supply_a_serial_for_psx_exe_fallback() {
    let iso = write_iso(vec![
        IsoFile::new("SYSTEM.CNF", b"BOOT = cdrom:\\SLUS_123.45;1\n".to_vec()),
        IsoFile::new("PSX.EXE", ps1_executable()),
    ]);
    let info = game_info(iso.path()).unwrap();
    assert_eq!(info.serial, None);
    assert_eq!(info.identity_md5, "501f5882147cac7900455053a479b219");
}

fn serial_sfo() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"\0PSF");
    for value in [0x101u32, 36, 44, 1] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(&0u16.to_le_bytes()); // key offset
    data.extend_from_slice(&0x0204u16.to_le_bytes()); // UTF-8 string
    // Data length, capacity, relative offset.
    for value in [10u32, 12, 0] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(b"DISC_ID\0");
    data.extend_from_slice(b"ULUS12345\0\0\0");
    data
}

#[test]
fn psp_serial_comes_from_sfo_in_iso_and_pbp() {
    let sfo = serial_sfo();
    let iso = write_iso(vec![
        IsoFile::new("PSP_GAME/PARAM.SFO", sfo.clone()),
        IsoFile::new("PSP_GAME/SYSDIR/EBOOT.BIN", b"executable".to_vec()),
    ]);
    let info = game_info(iso.path()).unwrap();
    assert_eq!(info.platform, "psp");
    assert_eq!(info.serial.as_deref(), Some("ULUS12345"));
    assert_eq!(info.retroachievements_platform_id, 41);
    assert_eq!(info.identity_md5, "c9962e8966477619f68f9d04dfd0e828");

    let mut pbp = Vec::from(&b"\0PBP\0\0\x01\0"[..]);
    pbp.extend_from_slice(&40u32.to_le_bytes());
    for _ in 1..8 {
        pbp.extend_from_slice(&(40u32 + sfo.len() as u32).to_le_bytes());
    }
    pbp.extend_from_slice(&sfo);
    let file = write_bytes("pbp", &pbp);
    let info = game_info(file.path()).unwrap();
    assert_eq!(info.platform, "psp");
    assert_eq!(info.serial.as_deref(), Some("ULUS12345"));
    assert_eq!(info.retroachievements_platform_id, 41);
    assert_eq!(info.identity_md5, "93d615faa6dc66332a8bce2f53241d5a");

    // Invalid metadata must not make an otherwise hashable PBP fail.
    pbp[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    let file = write_bytes("pbp", &pbp);
    assert_eq!(game_info(file.path()).unwrap().serial, None);
}
