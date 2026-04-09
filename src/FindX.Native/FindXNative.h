#pragma once
#include <cstdint>

#ifdef FINDX_EXPORTS
#define FINDX_API __declspec(dllexport)
#else
#define FINDX_API __declspec(dllimport)
#endif

extern "C" {

/// <param name="lastWriteTime">FILETIME (UTC) QuadPart，0 表示未知</param>
/// <param name="creationTime">FILETIME (UTC) QuadPart</param>
/// <param name="accessTime">FILETIME (UTC) QuadPart</param>
typedef void(__stdcall* FindXEnumCallback)(
    uint64_t fileRef,
    uint64_t parentRef,
    const wchar_t* fileName,
    int fileNameLen,
    uint32_t attributes,
    uint64_t fileSize,
    int64_t lastWriteTime,
    int64_t creationTime,
    int64_t accessTime
);

typedef void(__stdcall* FindXJournalCallback)(
    uint32_t reason,
    uint64_t fileRef,
    uint64_t parentRef,
    const wchar_t* fileName,
    int fileNameLen,
    uint32_t attributes,
    uint64_t fileSize,
    int64_t lastWriteTime,
    int64_t creationTime,
    int64_t accessTime
);

FINDX_API int __stdcall FindX_EnumVolume(
    wchar_t driveLetter,
    FindXEnumCallback callback,
    uint64_t* outNextUsn
);

FINDX_API int __stdcall FindX_ReadJournal(
    wchar_t driveLetter,
    uint64_t startUsn,
    FindXJournalCallback callback,
    uint64_t* outNextUsn
);

FINDX_API int __stdcall FindX_QueryJournal(
    wchar_t driveLetter,
    uint64_t* outJournalId,
    uint64_t* outNextUsn,
    uint64_t* outLowestUsn
);

/// 诊断：尝试打开卷并查询 USN Journal。返回 0=成功；-1=CreateFile 失败（*openErr=GetLastError）；-2=打开成功但 FSCTL_QUERY_USN_JOURNAL 失败（*journalErr）。
FINDX_API int __stdcall FindX_DiagnoseVolume(
    wchar_t driveLetter,
    uint32_t* openErr,
    uint32_t* journalErr
);

}
