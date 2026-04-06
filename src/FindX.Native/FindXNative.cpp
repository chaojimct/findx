#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winioctl.h>
#include <cstdint>
#include <cstring>
#include "FindXNative.h"

#pragma comment(lib, "kernel32.lib")

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
            auto* rec = reinterpret_cast<USN_RECORD_V2*>(ptr);
            if (rec->RecordLength < sizeof(USN_RECORD_V2)) break;
            if (ptr + rec->RecordLength > end) break;

            auto* name = reinterpret_cast<const wchar_t*>(
                reinterpret_cast<BYTE*>(rec) + rec->FileNameOffset);
            int nameLen = rec->FileNameLength / sizeof(wchar_t);

            uint64_t fileRef = rec->FileReferenceNumber & 0x0000FFFFFFFFFFFF;
            uint64_t parentRef = rec->ParentFileReferenceNumber & 0x0000FFFFFFFFFFFF;

            callback(fileRef, parentRef, name, nameLen,
                     rec->FileAttributes, 0, rec->TimeStamp.QuadPart);
            totalEntries++;

            ptr += rec->RecordLength;
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
            auto* rec = reinterpret_cast<USN_RECORD_V2*>(ptr);
            if (rec->RecordLength < sizeof(USN_RECORD_V2)) break;
            if (ptr + rec->RecordLength > end) break;

            auto* name = reinterpret_cast<const wchar_t*>(
                reinterpret_cast<BYTE*>(rec) + rec->FileNameOffset);
            int nameLen = rec->FileNameLength / sizeof(wchar_t);

            uint64_t fileRef = rec->FileReferenceNumber & 0x0000FFFFFFFFFFFF;
            uint64_t parentRef = rec->ParentFileReferenceNumber & 0x0000FFFFFFFFFFFF;

            callback(rec->Reason, fileRef, parentRef, name, nameLen,
                     rec->FileAttributes);
            totalEntries++;

            ptr += rec->RecordLength;
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
