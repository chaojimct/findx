//! Decisions for the MFT absence witness. No Windows handles live here, so the
//! rules can be tested without a volume.
//!
//! A `GENERIC_READ` handle on `\\.\C:` is a DASD handle. `OpenFileById` on that
//! handle returns `STATUS_INVALID_PARAMETER` (`0xC000000D` / win32 87) for a live
//! base file. The id hint is a separate access-0 handle. Reserved metadata and
//! extension records (nonzero base reference) are not `OpenFileById` targets; a
//! second record read corroborates them. A failed open never advances a cursor.

pub const INDEX_MASK: u64 = 0x0000_ffff_ffff_ffff;
pub const RESERVED_MFT_ORDINALS: u64 = 16;

pub fn frn_ordinal(id: u64) -> u64 {
    id & INDEX_MASK
}

pub fn frn_sequence(id: u64) -> u16 {
    (id >> 48) as u16
}

/// User files can be reopened with `FILE_READ_ATTRIBUTES`. Ordinals below 16
/// are reserved metadata (`$MFT` is 0) and must not be passed to `OpenFileById`.
pub fn witness_openable(candidate: u64) -> bool {
    frn_ordinal(candidate) >= RESERVED_MFT_ORDINALS && frn_sequence(candidate) != 0
}

/// A base file with a nonzero sequence can be opened. Reserved ordinals and
/// extension records (nonzero base MFT index) cannot.
pub fn direct_open_witness(ordinal: u64, sequence: u16, base: u64) -> bool {
    ordinal >= RESERVED_MFT_ORDINALS && sequence != 0 && base == 0
}

pub fn base_ordinal(bytes: &[u8]) -> Result<u64, &'static str> {
    if bytes.len() < 12 + 0x28 {
        return Err("truncated MFT base reference");
    }
    let raw = u64::from_le_bytes(bytes[12 + 0x20..12 + 0x28].try_into().unwrap());
    Ok(raw & INDEX_MASK)
}

pub fn witness_failure(candidate: u64, base: u64, win32: u32, native: u32) -> String {
    format!(
        "MFT witness unavailable frn={candidate:#x} ordinal={} sequence={} base={base:#x} win32={win32} ntstatus={native:#x}; id-hint access=0; cursor retained",
        frn_ordinal(candidate),
        frn_sequence(candidate)
    )
}

/// A reserved in-use record proves a higher slot is free only when a second
/// record read returns the same full FRN and that ordinal is strictly lower.
/// Header-only identity of an in-use reserved metadata record. Attribute bodies
/// are not read. Ordinal 0 (`$MFT`) is accepted here and rejected by `OpenFileById`.
pub fn parse_reserved_record(record_bytes: u32, bytes: &[u8]) -> Result<(u64, u16), &'static str> {
    let size = record_bytes as usize;
    if !(512..=65536).contains(&size) || !size.is_power_of_two() || bytes.len() != 12 + size {
        return Err("truncated reserved MFT record");
    }
    if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != size {
        return Err("reserved MFT record size mismatch");
    }
    let returned = u64::from_le_bytes(bytes[..8].try_into().unwrap()) & INDEX_MASK;
    let record = &bytes[12..];
    if &record[..4] != b"FILE" {
        return Err("reserved MFT record magic mismatch");
    }
    let sequence = u16::from_le_bytes(record[16..18].try_into().unwrap());
    let flags = u16::from_le_bytes(record[22..24].try_into().unwrap());
    let ordinal = u32::from_le_bytes(record[44..48].try_into().unwrap()) as u64;
    if sequence == 0 || flags & 1 == 0 || ordinal != returned || ordinal >= RESERVED_MFT_ORDINALS {
        return Err("reserved MFT record is not an in-use metadata file");
    }
    Ok((ordinal, sequence))
}

pub fn reserved_record_corroborates(
    requested: u64,
    witness: u64,
    again_ordinal: u64,
    again_sequence: u16,
) -> Result<bool, &'static str> {
    let ordinal = frn_ordinal(witness);
    let sequence = frn_sequence(witness);
    if ordinal >= RESERVED_MFT_ORDINALS {
        return Err("reserved witness was an ordinary file");
    }
    if sequence == 0 || again_sequence == 0 || ordinal != again_ordinal || sequence != again_sequence
    {
        return Err("reserved MFT witness was not corroborated");
    }
    let requested_ordinal = frn_ordinal(requested);
    if requested_ordinal == 0 {
        return Err("unqualified NTFS identity");
    }
    if ordinal >= requested_ordinal {
        return Err("reserved record is not strictly below the requested slot");
    }
    Ok(true)
}

pub fn parse_witness_header(record_bytes: u32, bytes: &[u8]) -> Result<(u64, u16, u64), &'static str> {
    let (ordinal, sequence) = {
        let size = record_bytes as usize;
        if !(512..=65536).contains(&size) || !size.is_power_of_two() || bytes.len() != 12 + size {
            return Err("truncated MFT witness record");
        }
        if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != size {
            return Err("MFT witness record size mismatch");
        }
        let returned = u64::from_le_bytes(bytes[..8].try_into().unwrap()) & INDEX_MASK;
        let record = &bytes[12..];
        if &record[..4] != b"FILE" {
            return Err("MFT witness record magic mismatch");
        }
        let sequence = u16::from_le_bytes(record[16..18].try_into().unwrap());
        let flags = u16::from_le_bytes(record[22..24].try_into().unwrap());
        let ordinal = u32::from_le_bytes(record[44..48].try_into().unwrap()) as u64;
        if sequence == 0 || flags & 1 == 0 || ordinal != returned {
            return Err("MFT witness record is not in use");
        }
        (ordinal, sequence)
    };
    let base = base_ordinal(bytes)?;
    Ok((ordinal, sequence, base))
}

/// A record that must not be passed to `OpenFileById` proves a higher slot is
/// free only when a second read returns the same ordinal, sequence and base,
/// and that ordinal is not above the requested slot. An ordinary base file is
/// rejected here so a failed open cannot be laundered into absence.
pub fn unopened_witness_corroborates(
    requested: u64,
    witness: u64,
    again_ordinal: u64,
    again_sequence: u16,
    base: u64,
    again_base: u64,
) -> Result<(), &'static str> {
    let ordinal = frn_ordinal(witness);
    let sequence = frn_sequence(witness);
    if direct_open_witness(ordinal, sequence, base) {
        return Err("ordinary base file must be opened");
    }
    if base != again_base
        || sequence == 0
        || again_sequence == 0
        || ordinal != again_ordinal
        || sequence != again_sequence
    {
        return Err("MFT witness was not corroborated");
    }
    let requested_ordinal = frn_ordinal(requested);
    if requested_ordinal == 0 || frn_sequence(requested) == 0 {
        return Err("unqualified NTFS identity");
    }
    if ordinal > requested_ordinal {
        return Err("witness ordinal is above the requested slot");
    }
    if ordinal == requested_ordinal && sequence == frn_sequence(requested) {
        return Err("witness is the requested identity and was not opened");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_access_is_not_how_a_witness_is_opened_and_ordinal_zero_is_not_opened() {
        assert!(!witness_openable(0));
        assert!(!witness_openable(1));
        assert!(!witness_openable((3 << 48) | 15));
        assert!(witness_openable((7 << 48) | 100));
        assert!(!witness_openable(100));
        let message = witness_failure((4 << 48) | 0, 0, 87, 0xc000_000d);
        assert!(message.contains("ordinal=0"));
        assert!(message.contains("sequence=4"));
        assert!(message.contains("base=0x0"));
        assert!(message.contains("win32=87"));
        assert!(message.contains("ntstatus=0xc000000d"));
        assert!(message.contains("id-hint access=0"));
        assert!(message.contains("cursor retained"));
        assert!(!direct_open_witness(0, 1, 0));
        assert!(!direct_open_witness(100, 9, 50));
        assert!(direct_open_witness(0x85571, 9, 0));
    }

    #[test]
    fn extension_record_is_corroborated_by_a_second_read_not_by_openfilebyid() {
        let requested = (9 << 48) | 0x85580;
        let witness = (3 << 48) | 0x85571;
        assert!(unopened_witness_corroborates(requested, witness, 0x85571, 3, 40, 40).is_ok());
        assert!(unopened_witness_corroborates(requested, witness, 0x85571, 4, 40, 40).is_err());
        assert!(unopened_witness_corroborates(requested, witness, 0x85571, 3, 40, 41).is_err());
        assert!(unopened_witness_corroborates(requested, witness, 0x85571, 3, 0, 0).is_err());
        assert!(unopened_witness_corroborates(witness, witness, 0x85571, 3, 40, 40).is_err());
        let mut bytes = vec![0u8; 12 + 512];
        bytes[..8].copy_from_slice(&0x85571u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&512u32.to_le_bytes());
        bytes[12..16].copy_from_slice(b"FILE");
        bytes[28..30].copy_from_slice(&3u16.to_le_bytes());
        bytes[34..36].copy_from_slice(&1u16.to_le_bytes());
        bytes[56..60].copy_from_slice(&0x85571u32.to_le_bytes());
        bytes[44..52].copy_from_slice(&40u64.to_le_bytes());
        assert_eq!(parse_witness_header(512, &bytes).unwrap(), (0x85571, 3, 40));
        assert_eq!(base_ordinal(&bytes).unwrap(), 40);
    }

    #[test]
    fn reserved_lower_record_proves_absence_only_when_the_second_read_matches() {
        let requested = (9 << 48) | 40;
        let mft = (1 << 48) | 0;
        assert!(reserved_record_corroborates(requested, mft, 0, 1).unwrap());
        assert!(reserved_record_corroborates(requested, mft, 0, 2).is_err());
        assert!(reserved_record_corroborates(requested, mft, 1, 1).is_err());
        assert!(reserved_record_corroborates((1 << 48) | 0, mft, 0, 1).is_err());
        assert!(reserved_record_corroborates(requested, (2 << 48) | 100, 100, 2).is_err());
        let mut bytes = vec![0u8; 12 + 512];
        bytes[..8].copy_from_slice(&0u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&512u32.to_le_bytes());
        bytes[12..16].copy_from_slice(b"FILE");
        bytes[28..30].copy_from_slice(&1u16.to_le_bytes());
        bytes[34..36].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(parse_reserved_record(512, &bytes).unwrap(), (0, 1));
        bytes[34] = 0;
        assert!(parse_reserved_record(512, &bytes).is_err());
    }
}
