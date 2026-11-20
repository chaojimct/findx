# Pure stop decision. No SCM, no pipe, no file IO.
# The installer version and the running image's --version are not a protocol2 receipt.
Set-StrictMode -Version Latest

function Test-FxTxServiceVersionProbe([string]$Text, [int]$ExitCode) {
 if ($ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($Text)) { return $false }
 $line = $null
 foreach ($part in ($Text -split '\r?\n')) {
  if (-not [string]::IsNullOrWhiteSpace($part)) { $line = $part.Trim(); break }
 }
 if (-not $line) { return $false }
 return $line -cmatch '^findx2-service \d+\.\d+\.\d+$'
}

function Test-FxTxIdentityRefusal([string]$Message) {
 if ([string]::IsNullOrWhiteSpace($Message)) { return $false }
 return $Message -match 'identity changed|not exact owned service PID|own-process service unavailable|administrator token required|Unexpected service image|Shared-process service'
}

function Get-FxTxCapabilityClass($Capabilities) {
 if ($null -eq $Capabilities) { return 'absent' }
 $names = @($Capabilities.PSObject.Properties.Name)
 $type = ''
 if ($names -contains 'type') { $type = [string]$Capabilities.type }
 if ($type -cne 'upgrade_capabilities_result') { return 'legacy-reply' }
 foreach ($field in @('protocol_version', 'durable_prepare', 'reason')) {
  if ($field -notin $names) { return 'malformed' }
 }
 if ($Capabilities.durable_prepare -isnot [bool]) { return 'malformed' }
 if ($Capabilities.reason -isnot [string] -or -not $Capabilities.reason) { return 'malformed' }
 if ($names -contains 'last_checkpoint_error' -and $null -ne $Capabilities.last_checkpoint_error -and $Capabilities.last_checkpoint_error -isnot [string]) { return 'malformed' }
 if ($names -contains 'runtime_error' -and $null -ne $Capabilities.runtime_error -and $Capabilities.runtime_error -isnot [string]) { return 'malformed' }
 if ($Capabilities.protocol_version -ne 2 -or -not $Capabilities.durable_prepare) { return 'legacy-reply' }
 $runtime = $null
 if ($names -contains 'runtime_error') { $runtime = $Capabilities.runtime_error }
 if ($null -ne $runtime -and "$runtime" -ne '') { return 'source-errors' }
 return 'protocol2'
}

function Get-FxTxPreStopMode {
 param(
  [bool]$ProbeOk,
  $Capabilities,
  [string]$CapabilitiesError = '',
  [string]$InstallerVersion = ''
 )
 # InstallerVersion is the package being installed. It must not choose the mode.
 $null = $InstallerVersion
 if (-not $ProbeOk) { return 'refuse-probe' }
 if (Test-FxTxIdentityRefusal $CapabilitiesError) { return 'refuse-identity' }
 switch (Get-FxTxCapabilityClass $Capabilities) {
  'protocol2' { return 'protocol2-attempt' }
  'source-errors' { return 'accepted-partial' }
  'legacy-reply' { return 'accepted-partial' }
  'absent' { return 'accepted-partial' }
  'malformed' { return 'refuse-malformed' }
  default { return 'refuse-malformed' }
 }
}

function Get-FxTxPrepareOutcome {
 param([string]$PrepareError = '', [bool]$TokenIssued = $false)
 if ($TokenIssued) { return 'protocol2-commit' }
 if ([string]::IsNullOrWhiteSpace($PrepareError)) { return 'protocol2-commit' }
 if ($PrepareError -match 'Bounded management response timeout|state may remain sealed') { return 'refuse-sealed' }
 if ($PrepareError -match 'wrong index or timeout bound') { return 'refuse-contract' }
 if ($PrepareError -match 'metadata not ready|watch/backfill unhealthy|unsupported management protocol|prepare timeout; RAM retained, no receipt') { return 'accepted-partial' }
 return 'refuse-sealed'
}

function Test-FxTxAcceptedStopExit([int]$Win32, [int]$Specific) {
 $accepted = ($Win32 -eq 0 -and $Specific -eq 0) -or ($Win32 -eq 1066 -and $Specific -eq 1)
 [pscustomobject]@{
  accepted = $accepted
  protocol2Receipt = $false
  savedWithSourceErrors = ($accepted -and $Specific -eq 1)
 }
}
