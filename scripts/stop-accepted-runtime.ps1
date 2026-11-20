# SCM Stop of the service that is already running. No prepare_stop, no new process,
# no exe swap, no kill. Protocol 2 cannot receipt this runtime while watch or
# metadata is unresolved; a service-specific exit 1 is the engine's own
# "saved RAM but source errors remain" result.
param(
    [string]$ServiceName = 'FindX2Search',
    [string]$Index = 'C:\ProgramData\FindX\index.bin',
    [string]$BackupRoot,
    [int]$TimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
function Fail([string]$Message) { throw $Message }
if (-not ([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Fail '需要现有管理员令牌。不发起新的 UAC，不调用 prepare_stop。'
}
$svc = Get-CimInstance Win32_Service -Filter ("Name='" + $ServiceName + "'")
if (-not $svc -or $svc.State -ne 'Running' -or $svc.ProcessId -eq 0) { Fail '服务不在 Running，没有执行停止。' }
$beforePid = [int]$svc.ProcessId
$v6 = $Index + '.v6'
$publishedPath = Join-Path $v6 'published.json'
if (-not (Test-Path -LiteralPath $publishedPath)) { Fail '停止前没有 published.json，不停止。' }
$beforeText = Get-Content -LiteralPath $publishedPath -Raw -Encoding UTF8
$before = $beforeText | ConvertFrom-Json
function Test-Generation([string]$Label) {
    $pointer = Get-Content -LiteralPath $publishedPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($pointer.format -ne 'v6-gen' -or $pointer.manifest.version -ne 6 -or $pointer.manifest.generation -lt 1) { Fail ($Label + ' published.json 不是 v6-gen') }
    $gen = [uint64]$pointer.manifest.generation
    $dir = Join-Path (Join-Path $v6 'gen') ([string]$gen)
    $manifestPath = Join-Path $dir 'manifest.json'
    if (-not (Test-Path -LiteralPath $manifestPath)) { Fail ($Label + ' 缺少代目录 manifest') }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ([uint64]$manifest.generation -ne $gen) { Fail ($Label + ' manifest 代数与指针不一致') }
    $names = @($pointer.files.PSObject.Properties.Name)
    if ($names.Count -lt 6) { Fail ($Label + ' 指针文件表不完整') }
    foreach ($name in $names) {
        $path = Join-Path $dir $name
        $item = Get-Item -LiteralPath $path
        $expect = [uint64]$pointer.files.PSObject.Properties[$name].Value.size
        if ([uint64]$item.Length -ne $expect) { Fail ($Label + ' 文件长度不符: ' + $name) }
    }
    return [pscustomobject]@{ generation = $gen; metadataReady = [bool]$pointer.manifest.metadata_ready; volumes = @($pointer.manifest.volumes); files = $names.Count; directory = $dir }
}
$beforeGen = Test-Generation '停止前'
$controller = [ServiceProcess.ServiceController]::new($ServiceName)
try { $controller.Stop() } finally { $controller.Dispose() }
$timer = [Diagnostics.Stopwatch]::StartNew()
$stopped = $null
do {
    $now = Get-CimInstance Win32_Service -Filter ("Name='" + $ServiceName + "'")
    $alive = Get-Process -Id $beforePid -ErrorAction SilentlyContinue
    if ($now.State -eq 'Stopped' -and -not $alive) { $stopped = $now; break }
    if ($now.State -eq 'Running' -and $now.ProcessId -ne $beforePid) { Fail '停止过程中服务被另一个进程重新拉起。不替换程序，不结束进程。' }
    Start-Sleep -Milliseconds 200
} while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds)
if (-not $stopped) { Fail 'SCM 仍未停止。不要结束进程，不要替换程序。' }
$exitCode = [int]$stopped.ExitCode
$specific = [int]$stopped.ServiceSpecificExitCode
$accepted = ($exitCode -eq 0 -and $specific -eq 0) -or ($exitCode -eq 1066 -and $specific -eq 1)
if (-not $accepted) { Fail ('停止退出码不是已保存的接受态: win32=' + $exitCode + ' specific=' + $specific) }
Start-Sleep -Seconds 2
if (Get-Process -Id $beforePid -ErrorAction SilentlyContinue) { Fail '原进程又出现了。不复制，不替换。' }
$after = Test-Generation '停止后'
if ($BackupRoot) {
    if (Test-Path -LiteralPath $BackupRoot) { Fail '备份目录已存在。换一个空路径，避免覆盖那次空的证据目录。' }
    New-Item -ItemType Directory -Path $BackupRoot | Out-Null
    Copy-Item -LiteralPath $Index -Destination (Join-Path $BackupRoot 'index.bin')
    Copy-Item -LiteralPath $v6 -Destination (Join-Path $BackupRoot 'index.bin.v6') -Recurse
    foreach ($side in @('index.bin.tri', 'index.bin.tri.pending', 'index.bin.exclude')) {
        $sidePath = Join-Path (Split-Path -Parent $Index) $side
        if (Test-Path -LiteralPath $sidePath) { Copy-Item -LiteralPath $sidePath -Destination (Join-Path $BackupRoot $side) }
    }
}
$result = [ordered]@{
    action = 'scm-stop'
    protocol2Receipt = $false
    prepareStopCalled = $false
    processKilled = $false
    exeReplaced = $false
    pid = $beforePid
    win32Exit = $exitCode
    serviceSpecificExit = $specific
    savedWithSourceErrors = ($specific -eq 1)
    generationBefore = $beforeGen.generation
    generationAfter = $after.generation
    metadataReady = $after.metadataReady
    volumes = $after.volumes
    fileCount = $after.files
    backupRoot = $BackupRoot
}
$json = $result | ConvertTo-Json -Depth 6
if ($BackupRoot) { Set-Content -LiteralPath (Join-Path $BackupRoot 'stop-accepted.json') -Value $json -Encoding UTF8 }
Write-Output $json
