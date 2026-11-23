//! Bounded NTFS identity absence proof. Never infer absence from an open error.
//! FSCTL_GET_NTFS_FILE_RECORD returns the nearest IN-USE ordinal <= requested.
//! Only bounded identity headers/fixup metadata are inspected. Attribute bodies
//! (which can contain resident data) are never interpreted, logged or persisted.
const INDEX_MASK: u64 = 0x0000_ffff_ffff_ffff;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VolumeData {
    pub serial: u64,
    pub record_bytes: u32,
    pub sector_bytes: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Epoch {
    pub volume: VolumeData,
    pub journal: u64,
}
fn verify_epoch(expected: Epoch, actual: Epoch) -> Result<(), String> {
    if expected != actual {
        Err("MFT volume/journal epoch changed; cursor retained".into())
    } else {
        Ok(())
    }
}
fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
/// `bytes` must be the actual bytes-returned slice, not the allocated buffer.
pub(crate) fn record_proves_absence(
    id: u64,
    volume: VolumeData,
    bytes: &[u8],
) -> Result<bool, String> {
    record_proves_absence_mode(id, volume, bytes, true)
}
fn record_proves_absence_mode(
    id: u64,
    volume: VolumeData,
    bytes: &[u8],
    check_fixups: bool,
) -> Result<bool, String> {
    let index = id & INDEX_MASK;
    let sequence = (id >> 48) as u16;
    if index == 0 || sequence == 0 {
        return Err("unqualified NTFS identity; cursor retained".into());
    }
    let size = volume.record_bytes as usize;
    let sector = volume.sector_bytes as usize;
    if !(512..=65536).contains(&size)
        || !size.is_power_of_two()
        || !(512..=4096).contains(&sector)
        || !sector.is_power_of_two()
        || size % sector != 0
        || bytes.len() < 12
    {
        return Err("invalid bounded MFT geometry/header".into());
    }
    let returned = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let ordinal = returned & INDEX_MASK;
    if ordinal > index {
        return Err("MFT returned ordinal beyond request".into());
    }
    if u32_at(bytes, 8) as usize != size || bytes.len() != 12 + size {
        return Err("truncated/oversized MFT response".into());
    }
    let r = &bytes[12..];
    if &r[..4] != b"FILE" || u32_at(r, 28) as usize != size || u32_at(r, 44) != ordinal as u32 {
        return Err("MFT record/header identity mismatch".into());
    }
    let seq = u16_at(r, 16);
    let flags = u16_at(r, 22);
    let used = u32_at(r, 24) as usize;
    let usa = u16_at(r, 4) as usize;
    let count = u16_at(r, 6) as usize;
    let end = usa
        .checked_add(count.checked_mul(2).ok_or("MFT USA overflow")?)
        .ok_or("MFT USA overflow")?;
    if seq == 0
        || flags & 1 == 0
        || flags & !3 != 0
        || (returned >> 48 != 0 && returned >> 48 != seq as u64)
        || count != size / sector + 1
        || usa < 48
        || end > used
        || used > size
        || (u16_at(r, 20) as usize) < end
        || (u16_at(r, 20) as usize) > used
    {
        return Err("invalid/inactive MFT record or update-sequence array".into());
    }
    // Windows may return raw MST tails or already-restored tails. Reject neither
    // valid representation, but require each sector to agree with its USA entry.
    let tag = u16_at(r, usa);
    for n in 1..count {
        let tail = u16_at(r, n * sector - 2);
        if check_fixups && tail != tag && tail != u16_at(r, usa + n * 2) {
            return Err("MFT sector fixup mismatch".into());
        }
    }
    // A lower in-use ordinal proves the requested slot was free at the query.
    // An exact ordinal with a different sequence proves ONLY the old full FRN
    // expired. Consumers delete that old full FRN, never the ordinal/new identity.
    Ok(ordinal < index || seq != sequence)
}
// A decoded kernel MFT record can carry stale USA trailers. This exception is
// NOT an absence proof: only propose an exact, DIFFERENT sequence at the SAME
// ordinal. Native code must independently reopen that complete new identity and
// verify it on the bound volume before the old identity may be removed.
fn replacement_claim(id: u64, volume: VolumeData, bytes: &[u8]) -> Result<u64, String> {
    if !record_proves_absence_mode(id, volume, bytes, false)? {
        return Err("MFT still names current full identity; cursor retained".into());
    }
    let returned = u64::from_le_bytes(bytes[..8].try_into().unwrap()) & INDEX_MASK;
    if returned != id & INDEX_MASK {
        return Err("uncorroborated lower MFT record; cursor retained".into());
    }
    let replacement = (u16_at(&bytes[12..], 16) as u64) << 48 | returned;
    if replacement == id {
        return Err("MFT replacement equals old identity".into());
    }
    Ok(replacement)
}
fn replacement_matches(expected: u64, actual: u64, serial: u32, actual_serial: u32) -> bool {
    expected == actual && serial == actual_serial
}
#[cfg(windows)]
pub(crate) mod native {
    use super::*;
    use std::mem::size_of;
    use windows::Win32::{
        Foundation::HANDLE,
        System::{
            Ioctl::{
                FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER,
            },
            IO::DeviceIoControl,
        },
    };
    pub(crate) fn volume(handle: HANDLE) -> Result<VolumeData, String> {
        let mut data = NTFS_VOLUME_DATA_BUFFER::default();
        let mut got = 0;
        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_GET_NTFS_VOLUME_DATA,
                None,
                0,
                Some((&mut data as *mut NTFS_VOLUME_DATA_BUFFER).cast()),
                size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                Some(&mut got),
                None,
            )
        }
        .map_err(|e| format!("MFT volume query failed: {e}; cursor retained"))?;
        if got < size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32 {
            return Err("partial NTFS volume data".into());
        }
        let out = VolumeData {
            serial: data.VolumeSerialNumber as u64,
            record_bytes: data.BytesPerFileRecordSegment,
            sector_bytes: data.BytesPerSector,
        };
        if !(512..=65536).contains(&out.record_bytes)
            || !out.record_bytes.is_power_of_two()
            || !(512..=4096).contains(&out.sector_bytes)
            || !out.sector_bytes.is_power_of_two()
            || out.record_bytes % out.sector_bytes != 0
        {
            return Err("unsupported bounded NTFS geometry".into());
        }
        Ok(out)
    }
    #[cfg(test)]
    pub(crate) fn describe(handle: HANDLE, id: u64) -> Result<String, String> {
        let v = volume(handle)?;
        let ordinal = (id & INDEX_MASK) as i64;
        let mut out = vec![0u8; v.record_bytes as usize + 12];
        let mut got = 0;
        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_GET_NTFS_FILE_RECORD,
                Some((&ordinal as *const i64).cast()),
                8,
                Some(out.as_mut_ptr().cast()),
                out.len() as u32,
                Some(&mut got),
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        if got as usize != out.len() {
            return Err(format!("unexpected metadata length {got}"));
        }
        let r = &out[12..];
        let usa = u16_at(r, 4) as usize;
        let count = u16_at(r, 6) as usize;
        let used = u32_at(r, 24) as usize;
        let mut text=format!("requested={id:#x} returned={:#x} header_record={} sequence={} flags={:#x} used={used} allocated={} first_attribute={} usa_offset={usa} usa_count={count} sector_bytes={} parser={:?}",u64::from_le_bytes(out[..8].try_into().unwrap()),u32_at(r,44),u16_at(r,16),u16_at(r,22),u32_at(r,28),u16_at(r,20),v.sector_bytes,record_proves_absence(id,v,&out));
        if usa + count * 2 <= r.len() && count > 0 {
            for n in 1..count {
                let at = n * v.sector_bytes as usize - 2;
                if at + 2 > r.len() {
                    break;
                }
                let tail = u16_at(r, at);
                text.push_str(&format!(
                    " [sector={n} tail_within_used={} matches_tag={} matches_saved={}]",
                    at + 2 <= used,
                    tail == u16_at(r, usa),
                    tail == u16_at(r, usa + n * 2)
                ));
            }
        }
        Ok(text)
    }
    pub(crate) fn absent(
        handle: HANDLE,
        id: u64,
        expected: VolumeData,
        journal: u64,
    ) -> Result<bool, String> {
        let epoch = Epoch {
            volume: expected,
            journal,
        };
        let before = Epoch {
            volume: volume(handle)?,
            journal: unsafe { crate::usn::query_usn_journal(handle) }
                .map_err(|e| e.to_string())?
                .UsnJournalID,
        };
        verify_epoch(epoch, before)?;
        let ordinal = (id & INDEX_MASK) as i64;
        let mut out = vec![0u8; expected.record_bytes as usize + 12];
        let mut got = 0;
        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_GET_NTFS_FILE_RECORD,
                Some((&ordinal as *const i64).cast()),
                8,
                Some(out.as_mut_ptr().cast()),
                out.len() as u32,
                Some(&mut got),
                None,
            )
        }
        .map_err(|e| format!("exact MFT identity query failed: {e}; cursor retained"))?;
        if got as usize > out.len() {
            return Err("invalid MFT bytes returned".into());
        }
        let bytes = &out[..got as usize];
        let answer = match record_proves_absence(id, expected, bytes) {
            Ok(answer) => answer,
            Err(error) if error == "MFT sector fixup mismatch" => {
                let replacement = replacement_claim(id, expected, bytes)?;
                let fresh=crate::ntfs_namespace::open_file_by_id_diagnostic(handle,replacement).map_err(|e|format!("replacement identity unavailable win32={} ntstatus={:#x}; cursor retained",e.win32,e.last_ntstatus))?;
                let mut info =
                    windows::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION::default();
                let checked = unsafe {
                    windows::Win32::Storage::FileSystem::GetFileInformationByHandle(
                        fresh, &mut info,
                    )
                };
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(fresh);
                }
                checked.map_err(|e| {
                    format!("replacement identity metadata failed: {e}; cursor retained")
                })?;
                let actual = (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64;
                if !replacement_matches(
                    replacement,
                    actual,
                    expected.serial as u32,
                    info.dwVolumeSerialNumber,
                ) {
                    return Err("replacement identity/volume mismatch; cursor retained".into());
                }
                true
            }
            Err(error) => return Err(error),
        };
        let after = Epoch {
            volume: volume(handle)?,
            journal: unsafe { crate::usn::query_usn_journal(handle) }
                .map_err(|e| e.to_string())?
                .UsnJournalID,
        };
        verify_epoch(epoch, after)?;
        Ok(answer)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn volume() -> VolumeData {
        VolumeData {
            serial: 123,
            record_bytes: 1024,
            sector_bytes: 512,
        }
    }
    fn record(index: u64, seq: u16) -> Vec<u8> {
        let mut b = vec![0u8; 1036];
        b[..8].copy_from_slice(&index.to_le_bytes());
        b[8..12].copy_from_slice(&1024u32.to_le_bytes());
        let r = &mut b[12..];
        r[..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&48u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[16..18].copy_from_slice(&seq.to_le_bytes());
        r[20..22].copy_from_slice(&56u16.to_le_bytes());
        r[22..24].copy_from_slice(&1u16.to_le_bytes());
        r[24..28].copy_from_slice(&64u32.to_le_bytes());
        r[28..32].copy_from_slice(&1024u32.to_le_bytes());
        r[44..48].copy_from_slice(&(index as u32).to_le_bytes());
        for at in [48, 510, 1022] {
            r[at..at + 2].copy_from_slice(&0x55aau16.to_le_bytes());
        }
        r[50..52].copy_from_slice(&0x1122u16.to_le_bytes());
        r[52..54].copy_from_slice(&0x3344u16.to_le_bytes());
        b
    }
    #[test]
    fn different_serial_journal_or_geometry_never_proves_absence() {
        let e = Epoch {
            volume: volume(),
            journal: 7,
        };
        assert!(verify_epoch(e, e).is_ok());
        for bad in [
            Epoch { journal: 8, ..e },
            Epoch {
                volume: VolumeData {
                    serial: 456,
                    ..volume()
                },
                ..e
            },
            Epoch {
                volume: VolumeData {
                    record_bytes: 4096,
                    ..volume()
                },
                ..e
            },
        ] {
            assert!(verify_epoch(e, bad).is_err());
        }
    }
    #[test]
    fn declared_output_sequence_must_match_header() {
        let mut b = record(99, 3);
        b[..8].copy_from_slice(&((4u64 << 48) | 99).to_le_bytes());
        assert!(record_proves_absence((7 << 48) | 100, volume(), &b).is_err());
    }
    #[test]
    fn decoded_reused_record_requires_independent_complete_identity() {
        let old = (7u64 << 48) | 100;
        let new = (8u64 << 48) | 100;
        let mut b = record(100, 8);
        b[522] ^= 0x80;
        assert!(record_proves_absence(old, volume(), &b).is_err());
        assert_eq!(replacement_claim(old, volume(), &b).unwrap(), new);
        assert!(replacement_matches(new, new, 123, 123));
        assert!(!replacement_matches(new, old, 123, 123));
        assert!(!replacement_matches(new, new, 123, 124));
        assert!(replacement_claim(new, volume(), &b).is_err());
        let mut lower = record(99, 8);
        lower[522] ^= 0x80;
        assert!(replacement_claim(old, volume(), &lower).is_err());
        b[12] = 0;
        assert!(replacement_claim(old, volume(), &b).is_err());
    }
    #[test]
    fn current_live_identity_is_not_absent() {
        assert!(!record_proves_absence((7 << 48) | 100, volume(), &record(100, 7)).unwrap());
    }
    #[test]
    fn reused_slot_proves_old_sequence_only() {
        assert!(record_proves_absence((7 << 48) | 100, volume(), &record(100, 8)).unwrap());
        assert!(!record_proves_absence((8 << 48) | 100, volume(), &record(100, 8)).unwrap());
    }
    #[test]
    fn nearest_lower_in_use_record_proves_hole() {
        assert!(record_proves_absence((7 << 48) | 100, volume(), &record(99, 3)).unwrap());
    }
    #[test]
    fn higher_or_mismatched_record_never_proves_absence() {
        assert!(record_proves_absence((7 << 48) | 100, volume(), &record(101, 3)).is_err());
        let mut b = record(99, 3);
        b[56..60].copy_from_slice(&98u32.to_le_bytes());
        assert!(record_proves_absence((7 << 48) | 100, volume(), &b).is_err());
    }
    #[test]
    fn partial_corrupt_inactive_and_unqualified_rejected() {
        let id = (7 << 48) | 100;
        let b = record(99, 3);
        for n in [0, 8, 12, 100, 1035] {
            assert!(record_proves_absence(id, volume(), &b[..n]).is_err());
        }
        for at in [12, 18, 34, 40, 522] {
            let mut bad = b.clone();
            bad[at] ^= 0x80;
            assert!(
                record_proves_absence(id, volume(), &bad).is_err(),
                "offset {at}"
            );
        }
        let mut huge = b.clone();
        huge[36..40].copy_from_slice(&999999u32.to_le_bytes());
        assert!(record_proves_absence(id, volume(), &huge).is_err());
        assert!(record_proves_absence(100, volume(), &b).is_err());
    }
    #[test]
    fn restored_sector_tails_are_valid() {
        let mut b = record(99, 3);
        b[522..524].copy_from_slice(&0x1122u16.to_le_bytes());
        b[1034..1036].copy_from_slice(&0x3344u16.to_le_bytes());
        assert!(record_proves_absence((7 << 48) | 100, volume(), &b).unwrap());
    }
}

#[cfg(all(test, windows))]
mod native_owned_tests {
    use super::*;
    use std::{
        fs::OpenOptions,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        },
    };
    #[test]
    fn inspect_authorized_failure_identity_metadata() {
        let Some(raw) = std::env::var_os("FINDX2_MFT_DIAGNOSTIC_ID") else {
            return;
        };
        assert!(std::env::var_os("FINDX2_NATIVE_VOLUME_PROOF").is_some());
        let id = raw.to_string_lossy().parse::<u64>().unwrap();
        let handle = crate::usn::open_volume_handle(r"\\.\C:").unwrap();
        let result = native::describe(handle, id);
        let v = native::volume(handle).unwrap();
        let journal = unsafe { crate::usn::query_usn_journal(handle) }
            .unwrap()
            .UsnJournalID;
        let absence = native::absent(handle, id, v, journal);
        println!("T176_EXACT_FAILURE_ABSENCE_PROOF {absence:?}");
        assert_eq!(absence, Ok(true));
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        println!("T176_EXACT_FAILURE_METADATA {result:?}");
        assert!(result.is_ok());
    }
    #[test]
    fn inspect_authorized_pending_identities_metadata() {
        let Some(pairs) = std::env::var_os("FINDX2_PENDING_DIAGNOSTIC_PAIRS") else {
            return;
        };
        use std::os::windows::io::FromRawHandle;
        let volume = if std::env::var_os("FINDX2_NATIVE_VOLUME_PROOF").is_some() {
            unsafe {
                std::fs::File::from_raw_handle(
                    crate::usn::open_volume_handle(r"\\.\C:").unwrap().0 as _,
                )
            }
        } else {
            OpenOptions::new()
                .read(true)
                .share_mode(7)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
                .open(std::env::var_os("USERPROFILE").unwrap())
                .unwrap()
        };
        let h = HANDLE(volume.as_raw_handle() as _);
        let v = native::volume(h).unwrap();
        let journal = unsafe { crate::usn::query_usn_journal(h) }
            .unwrap()
            .UsnJournalID;
        let mut ids = std::collections::BTreeSet::new();
        for pair in pairs.to_string_lossy().split(',').take(64) {
            for raw in pair.split(':') {
                ids.insert(raw.parse::<u64>().unwrap());
            }
        }
        for id in ids {
            let opened = crate::ntfs_namespace::inspect_identity_for_test(h, id);
            let proof = if opened.is_err() {
                format!("{:?}", native::absent(h, id, v, journal))
            } else {
                "not_needed_live_identity_opened".into()
            };
            println!("T176_PENDING_ID id={id} hex={id:#x} opened={opened:?} absence={proof}");
        }
        drop(volume);
    }
    #[test]
    fn ordinary_owned_handle_never_attests_live_file_absent() {
        let root = std::env::temp_dir().join(format!(
            "findx-t176-mft-owned-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("owned.txt");
        std::fs::write(&path, b"owned MFT fixture").unwrap();
        let hint = OpenOptions::new()
            .read(true)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(&root)
            .unwrap();
        let file = OpenOptions::new()
            .read(true)
            .share_mode(7)
            .open(&path)
            .unwrap();
        // Explicitly enabled only by the authorized DEV elevated proof harness.
        // Ordinary fixture mode keeps using its owned-directory hint.
        use std::os::windows::io::FromRawHandle;
        let proof_volume = if std::env::var_os("FINDX2_NATIVE_VOLUME_PROOF").is_some() {
            assert!(root
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("c:"));
            let handle = crate::usn::open_volume_handle(r"\\.\C:").unwrap();
            Some(unsafe { std::fs::File::from_raw_handle(handle.0 as _) })
        } else {
            None
        };
        let proof_handle = proof_volume
            .as_ref()
            .map(|f| HANDLE(f.as_raw_handle() as _))
            .unwrap_or(HANDLE(hint.as_raw_handle() as _));
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle() as _), &mut info) }
            .unwrap();
        let id = (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64;
        let mut context = None;
        let result = (|| {
            let h = proof_handle;
            let v = native::volume(h)?;
            assert_eq!(
                v.serial as u32, info.dwVolumeSerialNumber,
                "NTFS volume serial must match normal handle serial"
            );
            println!(
                "T176_MFT_VOLUME serial64={:#x} serial32={:#x} record_bytes={} sector_bytes={}",
                v.serial, info.dwVolumeSerialNumber, v.record_bytes, v.sector_bytes
            );
            let journal = unsafe { crate::usn::query_usn_journal(h) }
                .map_err(|e| e.to_string())?
                .UsnJournalID;
            context = Some((v, journal));
            native::absent(h, id, v, journal)
        })();
        assert!(
            !matches!(result, Ok(true)),
            "a live owned identity cannot be absence proof"
        );
        println!(
            "T176_MFT_ORDINARY_HANDLE path={} result={result:?} no_privilege_changes=true",
            root.display()
        );
        drop(file);
        std::fs::remove_file(&path).unwrap();
        let h = proof_handle;
        match crate::ntfs_namespace::open_file_by_id_diagnostic(h, id) {
            Err(error) => {
                println!(
                    "T176_MFT_TERMINAL_OPEN win32={} ntstatus={:#010x} frn={:#x}",
                    error.win32, error.last_ntstatus, id
                );
                assert_eq!(error.win32, 87);
            }
            Ok(unexpected) => {
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(unexpected);
                }
                panic!("deleted owned identity unexpectedly reopened");
            }
        }
        let terminal = context
            .map(|(v, j)| native::absent(h, id, v, j))
            .unwrap_or_else(|| Err("volume context unavailable; cursor retained".into()));
        assert!(
            !matches!(terminal, Ok(false)),
            "deleted full identity must not be reported live"
        );
        if matches!(result, Ok(false)) {
            assert!(
                matches!(terminal, Ok(true)),
                "available native MFT path must prove terminal absence: {terminal:?}"
            );
        }
        println!("T176_MFT_TERMINAL_PROOF result={terminal:?}; unavailable native proof remains fail-closed");
        drop(hint);
        std::fs::remove_dir(root).unwrap();
    }
}
