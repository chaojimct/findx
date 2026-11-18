# Real ShellLink COM fixture, not serialized/fabricated shortcut bytes.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
function Initialize-FxShortcutInterop {
 if('FxFixture.ShellLink' -as [type]){return}
 Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
namespace FxFixture {
 [ComImport, Guid("000214F9-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
 interface ILink {
  void GetPath([Out,MarshalAs(UnmanagedType.LPWStr)] StringBuilder path,int size,IntPtr data,uint flags);
  void GetIDList(out IntPtr pidl); void SetIDList(IntPtr pidl);
  void GetDescription([Out,MarshalAs(UnmanagedType.LPWStr)] StringBuilder text,int size);
  void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string text);
  void GetWorkingDirectory([Out,MarshalAs(UnmanagedType.LPWStr)] StringBuilder path,int size);
  void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string path);
  void GetArguments([Out,MarshalAs(UnmanagedType.LPWStr)] StringBuilder text,int size);
  void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string text);
  void GetHotkey(out short hotkey);void SetHotkey(short hotkey);
  void GetShowCmd(out int cmd);void SetShowCmd(int cmd);
  void GetIconLocation([Out,MarshalAs(UnmanagedType.LPWStr)] StringBuilder path,int size,out int index);
  void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path,int index);
  void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path,uint reserved);
  void Resolve(IntPtr window,uint flags);
  void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
 }
 public static class ShellLink {
  static object NewLink(){return Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("00021401-0000-0000-C000-000000000046")));}
  public static void Create(string target,string link){
   object obj=NewLink();try { ILink shell=(ILink)obj;shell.SetPath(target);shell.SetArguments("--help");shell.SetDescription("old owned fixture");((IPersistFile)obj).Save(link,true); } finally {Marshal.FinalReleaseComObject(obj);}
  }
  public static string Read(string path){
   object obj=NewLink();try {((IPersistFile)obj).Load(path,0);var text=new StringBuilder(32768);((ILink)obj).GetPath(text,text.Capacity,IntPtr.Zero,4);return text.ToString();} finally {Marshal.FinalReleaseComObject(obj);}
  }
 }
}
'@
}
function New-FxNativeShortcutFixture([string]$OwnedRoot,[string]$Target,[string]$Link){
 $root=[IO.Path]::GetFullPath($OwnedRoot).TrimEnd('\')+'\'
 $targetPath=(Resolve-Path -LiteralPath $Target -ErrorAction Stop).ProviderPath
 $linkPath=[IO.Path]::GetFullPath($Link)
 if(-not $targetPath.StartsWith($root,[StringComparison]::OrdinalIgnoreCase) -or -not $linkPath.StartsWith($root,[StringComparison]::OrdinalIgnoreCase) -or -not [IO.File]::Exists($targetPath) -or $targetPath -match '["\r\n]' -or [IO.Path]::GetExtension($linkPath) -ine '.lnk'){throw 'Not an existing file/real shortcut in the exact owned root'}
 if(-not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($linkPath)) -or [IO.File]::Exists($linkPath)){throw 'New shortcut fixture directory/identity invalid'}
 Initialize-FxShortcutInterop
 [FxFixture.ShellLink]::Create([string]$targetPath,[string]$linkPath)
 if(-not [IO.File]::Exists($linkPath) -or [FxFixture.ShellLink]::Read($linkPath) -ine $targetPath){throw 'Real native ShellLink COM persistence roundtrip failed'}
}
function Test-FxWshShortcutSupport([string]$OwnedRoot,[string]$Target,[string]$Evidence){
 $targetPath=(Resolve-Path -LiteralPath $Target -ErrorAction Stop).ProviderPath
 $root=[IO.Path]::GetFullPath($OwnedRoot).TrimEnd('\')+'\'
 if(-not $targetPath.StartsWith($root,[StringComparison]::OrdinalIgnoreCase) -or -not [IO.File]::Exists($targetPath) -or -not ([IO.Path]::GetFullPath($Evidence)).StartsWith($root,[StringComparison]::OrdinalIgnoreCase)){throw 'COM diagnostic outside exact owned root'}
 $rows=@();$ws=$null
 try{$ws=New-Object -ComObject WScript.Shell}catch{$rows+=@{stage='Create WScript.Shell';error=$_.Exception.ToString()}}
 if($ws){
 foreach($mode in @('direct-string','reflection-string')){
  $step='CreateShortcut';$row=[ordered]@{mode=$mode;target=$targetPath;targetType=$targetPath.GetType().FullName;targetLength=$targetPath.Length;exists=[IO.File]::Exists($targetPath);directory=[IO.Directory]::Exists($targetPath);null=($null -eq $targetPath);host=$PSVersionTable.PSVersion.ToString();apartment=[Threading.Thread]::CurrentThread.ApartmentState.ToString();shellType=$ws.GetType().FullName;passed=$false;step='';error=''}
  try{
   $link=Join-Path $OwnedRoot ('wsh-probe-'+$mode+'.lnk');$sc=$ws.CreateShortcut([string]$link)
   $row.Add('shortcutType',$sc.GetType().FullName);$row.Add('isCom',[Runtime.InteropServices.Marshal]::IsComObject($sc));$row.Add('propertyDefinition',($sc | Get-Member -Name TargetPath | Out-String))
   $step='TargetPath setter'
   if($mode -eq 'direct-string'){$sc.TargetPath=[string]$targetPath}else{$null=$sc.GetType().InvokeMember('TargetPath',[Reflection.BindingFlags]::SetProperty,$null,$sc,[object[]]@([string]$targetPath))}
   $step='Save';$sc.Save();$step='TargetPath getter';$actual=($ws.CreateShortcut([string]$link)).TargetPath
   if($actual -ine $targetPath){throw ('WSH actual target differs: '+$actual)};$row.passed=$true
  }catch{$row.error=$_.Exception.ToString()};$row.step=$step;$rows+=$row
 }
 }
 $rows | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 $Evidence
}
Export-ModuleMember -Function New-FxNativeShortcutFixture,Test-FxWshShortcutSupport
