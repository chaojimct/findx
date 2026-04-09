#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winioctl.h>
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <cwchar>
#include <unordered_map>
#include <vector>
#include "FindXNative.h"

#pragma comment(lib, "kernel32.lib")

struct FrnFileMeta {
    uint64_t size = 0;
    LONGLONG creationFt = 0;
    LONGLONG accessFt = 0;
    LONGLONG writeFt = 0;
};

/// NTFS 用「本记录内的 usa_count」描述覆盖几个扇区级块；步进应为 frsBytes/(usaCount-1)，
/// 不能盲信卷的 BytesPerSector（否则 1024 字节 FRS + 4096 逻辑扇区时整段校验永远失败，映射表为空）。
static bool ApplyNtfsFrsFixup(BYTE* frs, DWORD frsBytes)
{
    if (frsBytes < 48)
        return false;
    if (*reinterpret_cast<uint32_t*>(frs) != 0x454C4946) // 'FILE'
        return false;
    const WORD usaOff = *reinterpret_cast<uint16_t*>(frs + 0x04);
    const WORD usaCount = *reinterpret_cast<uint16_t*>(frs + 0x06);
    if (usaOff < sizeof(uint32_t) * 3 || usaCount < 2)
        return false;
    if (usaOff + usaCount * sizeof(uint16_t) > frsBytes)
        return false;

    const DWORD nProtected = static_cast<DWORD>(usaCount - 1);
    if (nProtected == 0 || frsBytes % nProtected != 0)
        return false;
    const DWORD sectorStep = frsBytes / nProtected;
    if (sectorStep < 2)
        return false;

    auto* usa = reinterpret_cast<uint16_t*>(frs + usaOff);
    const uint16_t usn = usa[0];
    for (WORD i = 1; i < usaCount; i++) {
        const size_t secEnd = static_cast<size_t>(i) * sectorStep;
        if (secEnd > frsBytes || secEnd < 2)
            return false;
        auto* tail = reinterpret_cast<uint16_t*>(frs + secEnd - 2);
        if (*tail != usn)
            return false;
        *tail = usa[i];
    }
    return true;
}

/// 自 resident STANDARD_INFORMATION / non-resident 无名 $DATA 取元数据；返回是否值得写入映射表
static bool ParseFrsFileMeta(BYTE* frs, DWORD frsLen, FrnFileMeta* m)
{
    m->size = 0;
    m->creationFt = m->accessFt = m->writeFt = 0;
    if (frsLen < 48)
        return false;
    if (*reinterpret_cast<uint32_t*>(frs) != 0x454C4946)
        return false;

    const uint16_t firstAttr = *reinterpret_cast<uint16_t*>(frs + 0x14);
    uint32_t bytesInUse = *reinterpret_cast<uint32_t*>(frs + 0x18);
    if (bytesInUse > frsLen)
        bytesInUse = frsLen;
    if (firstAttr < sizeof(uint32_t) * 3 || firstAttr >= bytesInUse)
        return false;

    bool haveStd = false;
    bool haveData = false;
    uint64_t dataSize = 0;

    for (uint32_t off = firstAttr; off + 24 <= bytesInUse;) {
        const uint32_t type = *reinterpret_cast<uint32_t*>(frs + off);
        if (type == 0xFFFFFFFF)
            break;
        const uint32_t alen = *reinterpret_cast<uint32_t*>(frs + off + 4);
        if (alen < 24 || off + alen > bytesInUse)
            break;

        const uint8_t nonRes = frs[off + 8];
        const uint8_t nameLen = frs[off + 9];

        if (type == 0x10 && nonRes == 0) {
            const uint32_t valLen = *reinterpret_cast<uint32_t*>(frs + off + 16);
            const uint16_t valOff = *reinterpret_cast<uint16_t*>(frs + off + 20);
            if (off + valOff + valLen <= bytesInUse && valLen >= 32) {
                const BYTE* v = frs + off + valOff;
                m->creationFt = *reinterpret_cast<const LONGLONG*>(v + 0);
                m->writeFt = *reinterpret_cast<const LONGLONG*>(v + 8);
                m->accessFt = *reinterpret_cast<const LONGLONG*>(v + 24);
                haveStd = true;
            }
        } else if (type == 0x80 && nameLen == 0 && nonRes == 0) {
            const uint32_t valLen = *reinterpret_cast<uint32_t*>(frs + off + 16);
            const uint16_t valOff = *reinterpret_cast<uint16_t*>(frs + off + 20);
            if (off + valOff + valLen <= bytesInUse) {
                dataSize = valLen;
                haveData = true;
            }
        } else if (type == 0x80 && nameLen == 0 && nonRes != 0) {
            const uint16_t nameOff = *reinterpret_cast<uint16_t*>(frs + off + 10);
            size_t headerEnd = 16;
            if (nameLen > 0)
                headerEnd = (static_cast<size_t>(nameOff) + static_cast<size_t>(nameLen) * 2 + 7) & ~size_t(7);
            const size_t dsOff = off + headerEnd + 32;
            if (dsOff + sizeof(uint64_t) <= bytesInUse) {
                dataSize = *reinterpret_cast<uint64_t*>(frs + dsOff);
                haveData = true;
            }
        }

        off += alen;
    }

    if (haveStd) {
        if (haveData)
            m->size = dataSize;
        return true;
    }
    return false;
}

/// 顺序读取 \\?\\X:\\$MFT，一次性建立 FRN -> 大小/时间（FILETIME）
static void LoadNtfsMftMetaMap(wchar_t driveLetter, const NTFS_VOLUME_DATA_BUFFER& vd,
    std::unordered_map<uint64_t, FrnFileMeta>& out)
{
    out.clear();

    wchar_t dl = driveLetter;
    if (dl >= L'a' && dl <= L'z')
        dl = static_cast<wchar_t>(dl - L'a' + L'A');

    wchar_t mftPath[32]{};
    _snwprintf_s(mftPath, _TRUNCATE, L"\\\\?\\%c:\\$MFT", dl);

    auto openMft = [](const wchar_t* p) {
        return CreateFileW(
            p,
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_SEQUENTIAL_SCAN,
            nullptr);
    };

    HANDLE hf = openMft(mftPath);
    if (hf == INVALID_HANDLE_VALUE) {
        wchar_t alt[16]{};
        _snwprintf_s(alt, _TRUNCATE, L"%c:\\$MFT", dl);
        hf = openMft(alt);
    }
    if (hf == INVALID_HANDLE_VALUE)
        return;

    const DWORD frs = vd.BytesPerFileRecordSegment;
    if (frs < 512 || frs > 65536) {
        CloseHandle(hf);
        return;
    }

    LARGE_INTEGER fileSize{};
    if (!GetFileSizeEx(hf, &fileSize)) {
        CloseHandle(hf);
        return;
    }

    LONGLONG limit = fileSize.QuadPart;
    if (vd.MftValidDataLength.QuadPart > 0)
        limit = (std::min)(limit, vd.MftValidDataLength.QuadPart);

    const size_t readChunk = 4u * 1024 * 1024;
    std::vector<BYTE> pending;
    pending.reserve(readChunk + frs);

    uint64_t frIndex = 0;
    if (limit > 0 && frs > 0) {
        const LONGLONG est = limit / static_cast<LONGLONG>(frs);
        const LONGLONG cap = static_cast<LONGLONG>(12) * 1000 * 1000;
        if (est > 0)
            out.reserve(static_cast<size_t>((std::min)(est, cap)));
    }

    LONGLONG readPos = 0;
    while (readPos < limit) {
        const DWORD toAsk = static_cast<DWORD>(
            (std::min)(static_cast<LONGLONG>(readChunk), limit - readPos));
        std::vector<BYTE> chunk(toAsk);
        DWORD got = 0;
        if (!ReadFile(hf, chunk.data(), toAsk, &got, nullptr) || got == 0)
            break;
        readPos += got;

        pending.insert(pending.end(), chunk.begin(), chunk.begin() + got);

        while (pending.size() >= frs) {
            BYTE* r = pending.data();
            if (!ApplyNtfsFrsFixup(r, frs)) {
                frIndex++;
                pending.erase(pending.begin(), pending.begin() + frs);
                continue;
            }

            const uint16_t flags = *reinterpret_cast<uint16_t*>(r + 0x16);
            if ((flags & 0x01) == 0) {
                frIndex++;
                pending.erase(pending.begin(), pending.begin() + frs);
                continue;
            }

            const uint16_t seq = *reinterpret_cast<uint16_t*>(r + 0x10);
            const uint64_t frn = (frIndex & 0x0000FFFFFFFFFFFFULL) | (static_cast<uint64_t>(seq) << 48);

            FrnFileMeta fm{};
            if (ParseFrsFileMeta(r, frs, &fm)) {
                out[frn] = fm;
                out[frn & 0x0000FFFFFFFFFFFFULL] = fm;
            }

            frIndex++;
            pending.erase(pending.begin(), pending.begin() + frs);
        }
    }

    CloseHandle(hf);
}

static uint64_t FileId128ToLo64(const FILE_ID_128& id)
{
    uint64_t v = 0;
    std::memcpy(&v, id.Identifier, sizeof(v));
    return v;
}

/// 用卷句柄 + 文件 ID 取标准信息（大小与三类时间，时间为 FILETIME QuadPart）
static bool QueryFileMetaById(
    HANDLE vol,
    uint64_t frn64,
    const FILE_ID_128* id128,
    bool useExtended128,
    uint64_t* outSize,
    LONGLONG* outCreationFt,
    LONGLONG* outAccessFt,
    LONGLONG* outWriteFt)
{
    FILE_ID_DESCRIPTOR fid{};
    fid.dwSize = sizeof(fid);
    if (useExtended128 && id128) {
        fid.Type = ExtendedFileIdType;
        fid.ExtendedFileId = *id128;
    } else {
        fid.Type = FileIdType;
        fid.FileId.QuadPart = static_cast<LONGLONG>(frn64);
    }

    HANDLE h = OpenFileById(
        vol,
        &fid,
        FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    if (h == INVALID_HANDLE_VALUE)
        return false;

    FILE_STANDARD_INFO st{};
    FILE_BASIC_INFO bs{};
    const BOOL okSt = GetFileInformationByHandleEx(h, FileStandardInfo, &st, sizeof(st));
    const BOOL okBs = GetFileInformationByHandleEx(h, FileBasicInfo, &bs, sizeof(bs));
    CloseHandle(h);
    if (!okSt || !okBs)
        return false;

    if (outSize) *outSize = static_cast<uint64_t>(st.EndOfFile.QuadPart);
    if (outCreationFt) *outCreationFt = bs.CreationTime.QuadPart;
    if (outAccessFt) *outAccessFt = bs.LastAccessTime.QuadPart;
    if (outWriteFt) *outWriteFt = bs.LastWriteTime.QuadPart;
    return true;
}

static const DWORD IO_BUF_SIZE = 65536;

static wchar_t UpperDriveLetter(wchar_t c) {
    if (c >= L'a' && c <= L'z')
        return (wchar_t)(c - L'a' + L'A');
    return c;
}

static HANDLE TryOpenVolumePath(const wchar_t* path, DWORD desiredAccess, DWORD flagsAndAttributes, DWORD* firstErr)
{
    HANDLE h = CreateFileW(
        path,
        desiredAccess,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        OPEN_EXISTING,
        flagsAndAttributes,
        nullptr);
    if (h != INVALID_HANDLE_VALUE)
        return h;
    if (firstErr && *firstErr == 0)
        *firstErr = GetLastError();
    return INVALID_HANDLE_VALUE;
}

static HANDLE OpenVolume(wchar_t letter)
{
    wchar_t path[8] = L"\\\\.\\X:";
    path[4] = UpperDriveLetter(letter);
    DWORD firstErr = 0;
    HANDLE h;

    h = TryOpenVolumePath(path, GENERIC_READ | GENERIC_WRITE, FILE_ATTRIBUTE_NORMAL, &firstErr);
    if (h != INVALID_HANDLE_VALUE)
        return h;

    h = TryOpenVolumePath(path,
        FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE,
        FILE_ATTRIBUTE_NORMAL,
        &firstErr);
    if (h != INVALID_HANDLE_VALUE)
        return h;

    h = TryOpenVolumePath(path, GENERIC_READ | GENERIC_WRITE, 0, &firstErr);
    if (h != INVALID_HANDLE_VALUE)
        return h;

    SetLastError(firstErr);
    return INVALID_HANDLE_VALUE;
}

extern "C" {

FINDX_API int __stdcall FindX_QueryJournal(
    wchar_t driveLetter,
    uint64_t* outJournalId,
    uint64_t* outNextUsn,
    uint64_t* outLowestUsn)
{
    HANDLE vol = OpenVolume(driveLetter);
    if (vol == INVALID_HANDLE_VALUE) return -1;

    USN_JOURNAL_DATA_V0 jd{};
    DWORD cb = 0;
    BOOL ok = DeviceIoControl(vol, FSCTL_QUERY_USN_JOURNAL,
        nullptr, 0, &jd, sizeof(jd), &cb, nullptr);
    CloseHandle(vol);
    if (!ok) return -2;

    if (outJournalId)  *outJournalId  = jd.UsnJournalID;
    if (outNextUsn)    *outNextUsn    = jd.NextUsn;
    if (outLowestUsn)  *outLowestUsn  = jd.LowestValidUsn;
    return 0;
}

FINDX_API int __stdcall FindX_EnumVolume(
    wchar_t driveLetter,
    FindXEnumCallback callback,
    uint64_t* outNextUsn)
{
    HANDLE vol = OpenVolume(driveLetter);
    if (vol == INVALID_HANDLE_VALUE) return -1;

    USN_JOURNAL_DATA_V0 jd{};
    DWORD cb = 0;
    if (!DeviceIoControl(vol, FSCTL_QUERY_USN_JOURNAL,
        nullptr, 0, &jd, sizeof(jd), &cb, nullptr)) {
        CloseHandle(vol);
        return -2;
    }

    std::unordered_map<uint64_t, FrnFileMeta> metaMap;
    NTFS_VOLUME_DATA_BUFFER ntfsVol{};
    DWORD ntfsCb = 0;
    if (DeviceIoControl(vol, FSCTL_GET_NTFS_VOLUME_DATA, nullptr, 0, &ntfsVol, sizeof(ntfsVol), &ntfsCb, nullptr))
        LoadNtfsMftMetaMap(driveLetter, ntfsVol, metaMap);

    auto* buf = static_cast<BYTE*>(HeapAlloc(GetProcessHeap(), 0, IO_BUF_SIZE));
    if (!buf) { CloseHandle(vol); return -3; }

    MFT_ENUM_DATA_V0 med{};
    med.StartFileReferenceNumber = 0;
    med.LowUsn = 0;
    med.HighUsn = jd.NextUsn;

    int totalEntries = 0;

    while (DeviceIoControl(vol, FSCTL_ENUM_USN_DATA,
        &med, sizeof(med), buf, IO_BUF_SIZE, &cb, nullptr))
    {
        if (cb <= sizeof(USN)) break;
        auto* ptr = buf + sizeof(USN);
        auto* end = buf + cb;

        while (ptr < end) {
            if (static_cast<size_t>(end - ptr) < sizeof(USN_RECORD_COMMON_HEADER)) break;
            auto* hdr = reinterpret_cast<USN_RECORD_COMMON_HEADER*>(ptr);
            if (hdr->RecordLength < sizeof(USN_RECORD_COMMON_HEADER)) break;
            if (ptr + hdr->RecordLength > end) break;

            uint64_t fileRef = 0;
            uint64_t parentRef = 0;
            DWORD attr = 0;
            WORD fnLen = 0;
            WORD fnOff = 0;
            LONGLONG usnTs = 0;
            bool use128 = false;
            FILE_ID_128 fid128{};

            if (hdr->MajorVersion == 3) {
                if (hdr->RecordLength < sizeof(USN_RECORD_V3)) break;
                auto* rec = reinterpret_cast<USN_RECORD_V3*>(ptr);
                fid128 = rec->FileReferenceNumber;
                fileRef = FileId128ToLo64(rec->FileReferenceNumber);
                parentRef = FileId128ToLo64(rec->ParentFileReferenceNumber);
                attr = rec->FileAttributes;
                fnLen = rec->FileNameLength;
                fnOff = rec->FileNameOffset;
                usnTs = rec->TimeStamp.QuadPart;
                use128 = true;
            } else if (hdr->MajorVersion == 2) {
                if (hdr->RecordLength < sizeof(USN_RECORD_V2)) break;
                auto* rec = reinterpret_cast<USN_RECORD_V2*>(ptr);
                fileRef = static_cast<uint64_t>(rec->FileReferenceNumber);
                parentRef = static_cast<uint64_t>(rec->ParentFileReferenceNumber);
                attr = rec->FileAttributes;
                fnLen = rec->FileNameLength;
                fnOff = rec->FileNameOffset;
                usnTs = rec->TimeStamp.QuadPart;
                use128 = false;
            } else {
                ptr += hdr->RecordLength;
                continue;
            }

            auto* name = reinterpret_cast<const wchar_t*>(ptr + fnOff);
            int nameLen = fnLen / static_cast<int>(sizeof(wchar_t));

            uint64_t size = 0;
            LONGLONG crFt = 0, acFt = 0, lwFt = 0;
            bool haveMftMeta = false;
            if (!metaMap.empty()) {
                auto it = metaMap.find(fileRef);
                if (it == metaMap.end())
                    it = metaMap.find(fileRef & 0x0000FFFFFFFFFFFFULL);
                if (it == metaMap.end() && use128) {
                    const uint64_t idLo = FileId128ToLo64(fid128);
                    it = metaMap.find(idLo);
                    if (it == metaMap.end())
                        it = metaMap.find(idLo & 0x0000FFFFFFFFFFFFULL);
                }
                if (it != metaMap.end()) {
                    haveMftMeta = true;
                    size = it->second.size;
                    crFt = it->second.creationFt;
                    acFt = it->second.accessFt;
                    lwFt = it->second.writeFt;
                }
            }
            if (!haveMftMeta)
                lwFt = usnTs;

            callback(fileRef, parentRef, name, nameLen, attr, size, lwFt, crFt, acFt);
            totalEntries++;

            ptr += hdr->RecordLength;
        }

        med.StartFileReferenceNumber = *reinterpret_cast<USN*>(buf);
    }

    HeapFree(GetProcessHeap(), 0, buf);
    if (outNextUsn) *outNextUsn = jd.NextUsn;
    CloseHandle(vol);
    return totalEntries;
}

FINDX_API int __stdcall FindX_ReadJournal(
    wchar_t driveLetter,
    uint64_t startUsn,
    FindXJournalCallback callback,
    uint64_t* outNextUsn)
{
    HANDLE vol = OpenVolume(driveLetter);
    if (vol == INVALID_HANDLE_VALUE) {
        if (outNextUsn) *outNextUsn = startUsn;
        return -1;
    }

    USN_JOURNAL_DATA_V0 jd{};
    DWORD cb = 0;
    if (!DeviceIoControl(vol, FSCTL_QUERY_USN_JOURNAL,
        nullptr, 0, &jd, sizeof(jd), &cb, nullptr)) {
        CloseHandle(vol);
        if (outNextUsn) *outNextUsn = startUsn;
        return -2;
    }

    auto* buf = static_cast<BYTE*>(HeapAlloc(GetProcessHeap(), 0, IO_BUF_SIZE));
    if (!buf) {
        CloseHandle(vol);
        if (outNextUsn) *outNextUsn = startUsn;
        return -3;
    }

    READ_USN_JOURNAL_DATA_V0 rujd{};
    rujd.StartUsn = startUsn;
    rujd.ReasonMask = 0xFFFFFFFF;
    rujd.UsnJournalID = jd.UsnJournalID;

    int totalEntries = 0;

    while (DeviceIoControl(vol, FSCTL_READ_USN_JOURNAL,
        &rujd, sizeof(rujd), buf, IO_BUF_SIZE, &cb, nullptr))
    {
        if (cb <= sizeof(USN)) break;
        auto nextUsn = *reinterpret_cast<USN*>(buf);

        auto* ptr = buf + sizeof(USN);
        auto* end = buf + cb;

        while (ptr < end) {
            if (static_cast<size_t>(end - ptr) < sizeof(USN_RECORD_COMMON_HEADER)) break;
            auto* hdr = reinterpret_cast<USN_RECORD_COMMON_HEADER*>(ptr);
            if (hdr->RecordLength < sizeof(USN_RECORD_COMMON_HEADER)) break;
            if (ptr + hdr->RecordLength > end) break;

            uint64_t fileRef = 0;
            uint64_t parentRef = 0;
            DWORD attr = 0;
            WORD fnLen = 0;
            WORD fnOff = 0;
            uint32_t reason = 0;
            bool use128 = false;
            FILE_ID_128 fid128{};
            LONGLONG usnTs = 0;

            if (hdr->MajorVersion == 3) {
                if (hdr->RecordLength < sizeof(USN_RECORD_V3)) break;
                auto* rec = reinterpret_cast<USN_RECORD_V3*>(ptr);
                fid128 = rec->FileReferenceNumber;
                fileRef = FileId128ToLo64(rec->FileReferenceNumber);
                parentRef = FileId128ToLo64(rec->ParentFileReferenceNumber);
                attr = rec->FileAttributes;
                fnLen = rec->FileNameLength;
                fnOff = rec->FileNameOffset;
                reason = rec->Reason;
                usnTs = rec->TimeStamp.QuadPart;
                use128 = true;
            } else if (hdr->MajorVersion == 2) {
                if (hdr->RecordLength < sizeof(USN_RECORD_V2)) break;
                auto* rec = reinterpret_cast<USN_RECORD_V2*>(ptr);
                fileRef = static_cast<uint64_t>(rec->FileReferenceNumber);
                parentRef = static_cast<uint64_t>(rec->ParentFileReferenceNumber);
                attr = rec->FileAttributes;
                fnLen = rec->FileNameLength;
                fnOff = rec->FileNameOffset;
                reason = rec->Reason;
                usnTs = rec->TimeStamp.QuadPart;
                use128 = false;
            } else {
                ptr += hdr->RecordLength;
                continue;
            }

            auto* name = reinterpret_cast<const wchar_t*>(ptr + fnOff);
            int nameLen = fnLen / static_cast<int>(sizeof(wchar_t));

            uint64_t size = 0;
            LONGLONG crFt = 0, acFt = 0, lwFt = 0;
            bool metaOk = use128
                ? QueryFileMetaById(vol, 0, &fid128, true, &size, &crFt, &acFt, &lwFt)
                : QueryFileMetaById(vol, fileRef, nullptr, false, &size, &crFt, &acFt, &lwFt);
            if (!metaOk) {
                size = 0;
                crFt = acFt = 0;
                lwFt = usnTs;
            }

            callback(reason, fileRef, parentRef, name, nameLen, attr, size, lwFt, crFt, acFt);
            totalEntries++;

            ptr += hdr->RecordLength;
        }

        if (nextUsn == rujd.StartUsn) break;
        rujd.StartUsn = nextUsn;
    }

    // 必须写回「本轮 READ 消费到的续读游标」。误用 QUERY 的 NextUsn 会导致与内核脱节，
    // 表现为重复扫日志或跳过区间，索引条数异常爬升、内存常驻上涨。
    if (outNextUsn) *outNextUsn = rujd.StartUsn;

    HeapFree(GetProcessHeap(), 0, buf);
    CloseHandle(vol);
    return totalEntries;
}

FINDX_API int __stdcall FindX_DiagnoseVolume(
    wchar_t driveLetter,
    uint32_t* openErr,
    uint32_t* journalErr)
{
    if (openErr) *openErr = 0;
    if (journalErr) *journalErr = 0;

    HANDLE vol = OpenVolume(driveLetter);
    if (vol == INVALID_HANDLE_VALUE) {
        if (openErr) *openErr = GetLastError();
        return -1;
    }

    USN_JOURNAL_DATA_V0 jd{};
    DWORD cb = 0;
    BOOL ok = DeviceIoControl(vol, FSCTL_QUERY_USN_JOURNAL,
        nullptr, 0, &jd, sizeof(jd), &cb, nullptr);
    CloseHandle(vol);
    if (!ok) {
        if (journalErr) *journalErr = GetLastError();
        return -2;
    }
    return 0;
}

}
