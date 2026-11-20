//! Decisions for the MFT absence witness. No Windows handles live here, so the
//! rules can be tested without a volume.
//!
//! An in-use attribute-list extension (nonzero 8-byte base reference at FILE+0x20)
//! is not an `OpenFileById` target. A failed open of that exact record is not
//! absence and is not a deletion of its base file. Reserved metadata still uses
//! the lower-slot rule. A failed open never advances a cursor by itself.

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

pub fn witness_failure(candidate: u64, win32: u32, native: u32) -> String {
    format!(
        "MFT witness unavailable frn={candidate:#x} ordinal={} sequence={} win32={win32} ntstatus={native:#x}; cursor retained",
        frn_ordinal(candidate),
        frn_sequence(candidate)
    )
}

/// All 8 bytes at FILE+0x20. Bits 0..47 are the base record number and bits
/// 48..63 are its sequence. Neither half is discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseRef {
    pub raw: u64,
    pub ordinal: u64,
    pub sequence: u16,
}

pub fn parse_base_reference(bytes: &[u8]) -> Result<BaseRef, &'static str> {
    if bytes.len() < 12 + 0x28 {
        return Err("truncated MFT base reference");
    }
    let raw = u64::from_le_bytes(bytes[12 + 0x20..12 + 0x28].try_into().unwrap());
    Ok(BaseRef {
        raw,
        ordinal: raw & INDEX_MASK,
        sequence: (raw >> 48) as u16,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordHeader {
    pub ordinal: u64,
    pub sequence: u16,
    pub flags: u16,
    pub base: BaseRef,
}

pub fn parse_record_header(record_bytes: u32, bytes: &[u8]) -> Result<RecordHeader, &'static str> {
    let size = record_bytes as usize;
    if !(512..=65536).contains(&size) || !size.is_power_of_two() || bytes.len() != 12 + size {
        return Err("truncated MFT record header");
    }
    if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != size {
        return Err("MFT record size mismatch");
    }
    let returned = u64::from_le_bytes(bytes[..8].try_into().unwrap()) & INDEX_MASK;
    let record = &bytes[12..];
    if &record[..4] != b"FILE" {
        return Err("MFT record magic mismatch");
    }
    let sequence = u16::from_le_bytes(record[16..18].try_into().unwrap());
    let flags = u16::from_le_bytes(record[22..24].try_into().unwrap());
    let ordinal = u32::from_le_bytes(record[44..48].try_into().unwrap()) as u64;
    if ordinal != returned {
        return Err("MFT record ordinal mismatch");
    }
    Ok(RecordHeader {
        ordinal,
        sequence,
        flags,
        base: parse_base_reference(bytes)?,
    })
}

/// Corroborated target of an in-use extension. `base` is the full reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionTarget {
    pub base: u64,
    pub base_is_dir: bool,
}

/// The requested FRN is this exact in-use record, and three reads agree on the
/// full 8-byte base reference. `Ok(None)` means this is not that shape: a base
/// file, or a different slot/sequence, stays on the open/absence path.
/// An unqualified, self-based, or chained reference is an error. The cursor stays.
/// This never means the extension or its base was deleted.
pub fn corroborated_extension(
    requested: u64,
    record: RecordHeader,
    again: RecordHeader,
    base_record: RecordHeader,
    reread: RecordHeader,
) -> Result<Option<ExtensionTarget>, &'static str> {
    if record.ordinal != frn_ordinal(requested) || record.sequence != frn_sequence(requested) {
        return Ok(None);
    }
    if record.flags & 1 == 0 || record.sequence == 0 {
        return Err("requested MFT record is not in use");
    }
    if record.base.raw == 0 {
        return Ok(None);
    }
    if record.base.raw != again.base.raw
        || record.base.raw != reread.base.raw
        || record.ordinal != again.ordinal
        || record.ordinal != reread.ordinal
        || record.sequence != again.sequence
        || record.sequence != reread.sequence
        || record.flags != again.flags
        || record.flags != reread.flags
    {
        return Err("extension record changed between reads");
    }
    if record.base.ordinal == 0 || record.base.sequence == 0 {
        return Err("unqualified extension base reference");
    }
    if record.base.ordinal == record.ordinal {
        return Err("extension base reference points at itself");
    }
    if base_record.ordinal != record.base.ordinal || base_record.sequence != record.base.sequence {
        return Err("base record does not match the extension reference");
    }
    if base_record.flags & 1 == 0 || base_record.sequence == 0 {
        return Err("extension base record is not in use");
    }
    if base_record.flags & !3 != 0 {
        return Err("extension base flags are not a file or directory");
    }
    if base_record.base.raw != 0 {
        return Err("extension base is itself an extension");
    }
    Ok(Some(ExtensionTarget {
        base: record.base.raw,
        base_is_dir: base_record.flags & 2 != 0,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdRole {
    File,
    Parent,
    Delete,
    Metadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionAction {
    Keep,
    Drop,
    Retarget(u64),
    Retain(&'static str),
}

/// What a watcher may do with one id after a corroborated extension read.
/// Delete of the extension id is dropped and does not delete the base.
/// A parent is retargeted only when the base record is a directory.
/// A named file event is retargeted only when that name is already a hardlink
/// of the base. Anything else keeps the cursor.
pub fn extension_action(
    role: IdRole,
    target: Option<ExtensionTarget>,
    name_matches_base: bool,
) -> ExtensionAction {
    let Some(target) = target else {
        return ExtensionAction::Keep;
    };
    match role {
        IdRole::Delete => ExtensionAction::Drop,
        IdRole::Metadata => ExtensionAction::Retarget(target.base),
        IdRole::Parent if target.base_is_dir => ExtensionAction::Retarget(target.base),
        IdRole::Parent => ExtensionAction::Retain("in-use extension parent is not a directory"),
        IdRole::File if name_matches_base => ExtensionAction::Retarget(target.base),
        IdRole::File => ExtensionAction::Retain("in-use extension file name is not a base hardlink"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn header(ordinal: u64, sequence: u16, flags: u16, base_raw: u64) -> RecordHeader {
        RecordHeader {
            ordinal,
            sequence,
            flags,
            base: BaseRef {
                raw: base_raw,
                ordinal: base_raw & INDEX_MASK,
                sequence: (base_raw >> 48) as u16,
            },
        }
    }

    #[test]
    fn reserved_ordinal_zero_is_not_opened() {
        assert!(!witness_openable(0));
        assert!(!witness_openable(1));
        assert!(!witness_openable((3 << 48) | 15));
        assert!(witness_openable((7 << 48) | 100));
        assert!(!witness_openable(100));
        let message = witness_failure((4 << 48) | 0, 87, 0xc000_000d);
        assert!(message.contains("ordinal=0"));
        assert!(message.contains("sequence=4"));
        assert!(message.contains("win32=87"));
        assert!(message.contains("ntstatus=0xc000000d"));
        assert!(message.contains("cursor retained"));
    }

    #[test]
    fn live_extension_seq5_file_parent_retains() {
        // Second elevated probe at ce831ac. rawBase is the eight bytes
        // 67-55-08-00-00-00-05-00. The base record is sequence 5, flags 1.
        const REQUESTED: u64 = 0x0009_0000_0008_5571;
        let base_raw = u64::from_le_bytes([0x67, 0x55, 0x08, 0x00, 0x00, 0x00, 0x05, 0x00]);
        assert_eq!(base_raw, 0x0005_0000_0008_5567);
        let ext = header(0x85571, 9, 1, base_raw);
        let base = header(0x85567, 5, 1, 0);
        let found = corroborated_extension(REQUESTED, ext, ext, base, ext).unwrap().unwrap();
        assert_eq!(found.base, base_raw);
        assert!(!found.base_is_dir);
        assert_eq!(
            extension_action(IdRole::Parent, Some(found), false),
            ExtensionAction::Retain("in-use extension parent is not a directory")
        );
        assert_eq!(extension_action(IdRole::Delete, Some(found), false), ExtensionAction::Drop);
        assert_eq!(
            extension_action(IdRole::File, Some(found), false),
            ExtensionAction::Retain("in-use extension file name is not a base hardlink")
        );
        assert_eq!(
            extension_action(IdRole::File, Some(found), true),
            ExtensionAction::Retarget(base_raw)
        );
        assert_eq!(extension_action(IdRole::Metadata, Some(found), false), ExtensionAction::Retarget(base_raw));
        // A directory base is a different record, not this live file.
        let dir_raw = (5u64 << 48) | 0x10;
        let dir_ext = header(0x20, 1, 1, dir_raw);
        let dir_base = header(0x10, 5, 3, 0);
        let dir = corroborated_extension((1u64 << 48) | 0x20, dir_ext, dir_ext, dir_base, dir_ext)
            .unwrap()
            .unwrap();
        assert!(dir.base_is_dir);
        assert_eq!(extension_action(IdRole::Parent, Some(dir), false), ExtensionAction::Retarget(dir_raw));
        // Same low 48 bits with a different sequence is not the same reference.
        let drifted = header(0x85571, 9, 1, 0x85567);
        assert!(corroborated_extension(REQUESTED, ext, drifted, base, ext).is_err());
        // Sequence 0 in the high half is unqualified. It is not a retarget and
        // the caller must not turn it into absence.
        let unqualified = header(0x85571, 9, 1, 0x85567);
        assert!(corroborated_extension(REQUESTED, unqualified, unqualified, base, unqualified).is_err());
        assert_ne!(
            extension_action(IdRole::Delete, None, false),
            ExtensionAction::Drop
        );
        // A zero base reference is an ordinary file and stays on the open path.
        let ordinary = header(0x85571, 9, 1, 0);
        assert!(corroborated_extension(REQUESTED, ordinary, ordinary, base, ordinary).unwrap().is_none());
        // A different requested sequence is not this extension.
        assert!(corroborated_extension((8 << 48) | 0x85571, ext, ext, base, ext).unwrap().is_none());
        // The base record is itself an extension: do not walk further.
        let chained = header(0x85567, 4, 1, (1 << 48) | 10);
        assert!(corroborated_extension(REQUESTED, ext, ext, chained, ext).is_err());
        let mut bytes = vec![0u8; 12 + 1024];
        bytes[..8].copy_from_slice(&0x85571u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&1024u32.to_le_bytes());
        bytes[12..16].copy_from_slice(b"FILE");
        bytes[12 + 16..12 + 18].copy_from_slice(&9u16.to_le_bytes());
        bytes[12 + 22..12 + 24].copy_from_slice(&1u16.to_le_bytes());
        bytes[12 + 0x20..12 + 0x28].copy_from_slice(&base_raw.to_le_bytes());
        bytes[12 + 44..12 + 48].copy_from_slice(&0x85571u32.to_le_bytes());
        let parsed = parse_record_header(1024, &bytes).unwrap();
        assert_eq!(parsed.sequence, 9);
        assert_eq!(parsed.flags, 1);
        assert_eq!(parsed.ordinal, 0x85571);
        assert_eq!(parsed.base.raw, base_raw);
        assert_eq!(parsed.base.ordinal, 0x85567);
        assert_eq!(parsed.base.sequence, 5);
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
