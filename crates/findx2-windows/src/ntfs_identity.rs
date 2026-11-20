//! Bounded kernel MFT claim + independent full-FRN corroboration. No open error is absence.
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

#[derive(Debug)]
struct Claim { candidate:u64, absent:bool }
fn claim(id:u64,v:VolumeData,bytes:&[u8])->Result<Claim,String>{
    let absent=record_proves_absence_mode(id,v,bytes,false)?;
    let ordinal=u64::from_le_bytes(bytes[..8].try_into().unwrap())&INDEX_MASK;
    Ok(Claim{candidate:(u16_at(&bytes[12..],16) as u64)<<48|ordinal,absent})
}
fn corroborate(c:&Claim,v:VolumeData,witness:Option<(u64,u32)>)->Result<bool,String>{
    if witness!=Some((c.candidate,v.serial as u32)){return Err("uncorroborated MFT record; exact independent identity/volume required; cursor retained".into());}
    Ok(c.absent)
}
#[cfg(windows)]
use crate::ntfs_namespace::NamespaceResolveError as E;
#[cfg(windows)]
use windows::Win32::{Foundation::{HANDLE,CloseHandle},System::{IO::DeviceIoControl,Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA,FSCTL_GET_NTFS_FILE_RECORD,NTFS_VOLUME_DATA_BUFFER}},Storage::FileSystem::{GetFileInformationByHandle,BY_HANDLE_FILE_INFORMATION}};
#[cfg(windows)]
pub(crate) fn epoch(h:HANDLE)->Result<Epoch,E>{
    let mut b=NTFS_VOLUME_DATA_BUFFER::default();let mut got=0;
    unsafe{DeviceIoControl(h,FSCTL_GET_NTFS_VOLUME_DATA,None,0,Some((&mut b as *mut NTFS_VOLUME_DATA_BUFFER).cast()),std::mem::size_of_val(&b) as u32,Some(&mut got),None)}.map_err(|e|E::Retry(format!("NTFS geometry unavailable: {e}")))?;
    if got<std::mem::size_of_val(&b) as u32{return Err("partial NTFS geometry".into());}
    let volume=VolumeData{serial:b.VolumeSerialNumber as u64,record_bytes:b.BytesPerFileRecordSegment,sector_bytes:b.BytesPerSector};
    if !(512..=65536).contains(&volume.record_bytes)||!volume.record_bytes.is_power_of_two()||!(512..=4096).contains(&volume.sector_bytes)||!volume.sector_bytes.is_power_of_two()||volume.record_bytes%volume.sector_bytes!=0{return Err("invalid bounded NTFS geometry".into());}
    let journal=unsafe{crate::usn::query_usn_journal(h)}.map_err(|e|E::Retry(format!("journal unavailable: {e}")))?.UsnJournalID;
    Ok(Epoch{volume,journal})
}
#[cfg(windows)]
pub(crate) fn check(h:HANDLE,expected:Epoch)->Result<(),E>{
 let actual=epoch(h)?;
 if actual.volume!=expected.volume{return Err("native volume/geometry identity changed; never follow a foreign volume".into());}
 if actual.journal!=expected.journal{return Err(E::EpochChanged("native journal epoch changed; retained cursor requires replay/rebuild".into()));}
 Ok(())
}
#[cfg(windows)]
pub(crate) fn absent(data:HANDLE,hint:HANDLE,id:u64,expected:Epoch)->Result<bool,E>{
    check(data,expected)?;
    let ordinal=(id&INDEX_MASK) as i64;let mut bytes=vec![0u8;12+expected.volume.record_bytes as usize];let mut got=0;
    unsafe{DeviceIoControl(data,FSCTL_GET_NTFS_FILE_RECORD,Some((&ordinal as *const i64).cast()),8,Some(bytes.as_mut_ptr().cast()),bytes.len() as u32,Some(&mut got),None)}.map_err(|e|E::Retry(format!("MFT query unavailable: {e}; cursor retained")))?;
    if got as usize>bytes.len(){return Err("MFT output length exceeded bound".into());}
    let view=&bytes[..got as usize];
    let c=claim(id,expected.volume,view)?;
    let base=crate::ntfs_witness::base_ordinal(view).map_err(|reason|E::Retry(format!("{reason}; cursor retained")))?;
    // FSCTL returns the nearest in-use ordinal <= requested. A lower in-use
    // record is absence proof only after an independent check. Base files are
    // reopened on the access-0 id hint (not the DASD/GENERIC_READ handle).
    // Reserved metadata and extension records are not OpenFileById targets; a
    // second record read must return the same ordinal, sequence and base.
    let candidate_ordinal=crate::ntfs_witness::frn_ordinal(c.candidate);
    let candidate_sequence=crate::ntfs_witness::frn_sequence(c.candidate);
    if !crate::ntfs_witness::direct_open_witness(candidate_ordinal, candidate_sequence, base) {
        let slot = candidate_ordinal as i64;
        let mut bytes2 = vec![0u8; 12 + expected.volume.record_bytes as usize];
        let mut got2 = 0u32;
        unsafe { DeviceIoControl(data, FSCTL_GET_NTFS_FILE_RECORD, Some((&slot as *const i64).cast()), 8, Some(bytes2.as_mut_ptr().cast()), bytes2.len() as u32, Some(&mut got2), None) }
            .map_err(|e| { let code = e.code().0 as u32; E::Retry(format!("{}; witness reread failed: {e}", crate::ntfs_witness::witness_failure(c.candidate, base, code & 0xffff, code))) })?;
        if got2 as usize > bytes2.len() { return Err("MFT output length exceeded bound".into()); }
        let (again_ordinal, again_sequence, again_base) = crate::ntfs_witness::parse_witness_header(expected.volume.record_bytes, &bytes2[..got2 as usize])
            .map_err(|reason| E::Retry(format!("{}; {reason}", crate::ntfs_witness::witness_failure(c.candidate, base, 0, 0))))?;
        crate::ntfs_witness::unopened_witness_corroborates(id, c.candidate, again_ordinal, again_sequence, base, again_base)
            .map_err(|reason| E::Retry(format!("{}; {reason}", crate::ntfs_witness::witness_failure(c.candidate, base, 0, 0))))?;
        if !c.absent {
            return Err(E::Retry(format!("{}; unopened witness is not absence proof", crate::ntfs_witness::witness_failure(c.candidate, base, 0, 0))));
        }
        check(data, expected)?;
        return Ok(true);
    }
    let opened=super::ntfs_namespace::open_file_by_id_diagnostic(hint,c.candidate).map_err(|e|{
        let message=crate::ntfs_witness::witness_failure(c.candidate,base,e.win32,e.native);
        tracing::warn!("{message}");
        E::Retry(message)
    })?;
    let mut info=BY_HANDLE_FILE_INFORMATION::default();let queried=unsafe{GetFileInformationByHandle(opened,&mut info)};unsafe{let _=CloseHandle(opened);}
    queried.map_err(|e|E::Retry(format!("MFT witness metadata unavailable: {e}")))?;
    let actual=(info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64;
    let result=corroborate(&c,expected.volume,Some((actual,info.dwVolumeSerialNumber)))?;
    check(data,expected)?;Ok(result)
}
#[cfg(test)] mod ntfs_identity_tests {
    use super::*;
    fn volume()->VolumeData{VolumeData{serial:123,record_bytes:1024,sector_bytes:512}}
    fn record(index:u64,seq:u16)->Vec<u8>{
        let mut b=vec![0u8;1036];b[..8].copy_from_slice(&index.to_le_bytes());b[8..12].copy_from_slice(&1024u32.to_le_bytes());let r=&mut b[12..];r[..4].copy_from_slice(b"FILE");
        for (at,v) in [(4,48u16),(6,3),(16,seq),(20,56),(22,1),(48,0x55aa),(50,0x1122),(52,0x3344),(510,0x55aa),(1022,0x55aa)]{r[at..at+2].copy_from_slice(&v.to_le_bytes());}
        r[24..28].copy_from_slice(&64u32.to_le_bytes());r[28..32].copy_from_slice(&1024u32.to_le_bytes());r[44..48].copy_from_slice(&(index as u32).to_le_bytes());b
    }
    #[test]fn ntfs_identity_uncorroborated_lower_record_and_decoded_tail_never_erase_identity(){
        let id=(7<<48)|100;let mut b=record(99,3);b[522]^=0x80;
        assert!(record_proves_absence(id,volume(),&b).is_err());let c=claim(id,volume(),&b).unwrap();
        assert!(corroborate(&c,volume(),None).is_err());assert!(corroborate(&c,volume(),Some(((4<<48)|99,123))).is_err());assert!(corroborate(&c,volume(),Some((c.candidate,124))).is_err());
        assert!(corroborate(&c,volume(),Some((c.candidate,123))).unwrap());
    }
    #[test]fn ntfs_identity_live_reused_slot_and_qualified_sequence_are_distinct(){
        let id=(7<<48)|100;let c=claim(id,volume(),&record(100,7)).unwrap();assert!(!corroborate(&c,volume(),Some((id,123))).unwrap());
        let c=claim(id,volume(),&record(100,8)).unwrap();assert!(corroborate(&c,volume(),Some(((8<<48)|100,123))).unwrap());assert!(corroborate(&c,volume(),Some((id,123))).is_err());assert!(claim(100,volume(),&record(99,3)).is_err());
    }
    #[test]fn ntfs_identity_wrong_epoch_geometry_partial_and_header_reject(){
        let e=Epoch{volume:volume(),journal:7};assert!(verify_epoch(e,Epoch{journal:8,..e}).is_err());assert!(verify_epoch(e,Epoch{volume:VolumeData{serial:124,..volume()},..e}).is_err());
        let id=(7<<48)|100;let mut b=record(99,3);assert!(claim(id,volume(),&b[..1035]).is_err());b[56..60].copy_from_slice(&98u32.to_le_bytes());assert!(claim(id,volume(),&b).is_err());assert!(claim(id,volume(),&record(101,3)).is_err());
    }
}
