param(
  [Parameter(Mandatory = $true)]
  [string]$CandidateInstaller,

  [Parameter(Mandatory = $true)]
  [string]$CandidateVersion,

  [Parameter(Mandatory = $true)]
  [string]$InstallDirectory,

  [Parameter(Mandatory = $true)]
  [string]$DataDirectory,

  [string]$PreviousInstaller = '',
  [string]$PreviousTag = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-CandidateVersion {
  param([string]$Version)
  return [bool]($Version -match '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?$')
}

if (!(Test-CandidateVersion -Version $CandidateVersion)) {
  throw "CandidateVersion must be SemVer without build metadata, got: $CandidateVersion"
}
$CandidateInstaller = (Resolve-Path -LiteralPath $CandidateInstaller).Path
if ($PreviousInstaller) {
  $PreviousInstaller = (Resolve-Path -LiteralPath $PreviousInstaller).Path
}

function Wait-Dashboard {
  param(
    [string]$ExpectedVersion = '',
    [int]$Attempts = 60
  )
  foreach ($attempt in 1..$Attempts) {
    try {
      $html = (Invoke-WebRequest http://127.0.0.1:9042/dashboard/ -UseBasicParsing).Content
      if ($html -notmatch 'id="app"') { throw 'Dashboard HTML is incomplete' }
      if (!$ExpectedVersion) { return $null }
      $status = Invoke-RestMethod http://127.0.0.1:9042/dashboard/api/v3/settings/update-status
      if ($status.currentVersion -eq $ExpectedVersion) { return $status }
    } catch {}
    Start-Sleep 1
  }
  throw "Installed GUI did not expose dashboard version $ExpectedVersion"
}

function Get-V3Settings {
  return Invoke-RestMethod http://127.0.0.1:9042/dashboard/api/v3/settings
}

function Set-V3AutoStart {
  param([bool]$Enabled)
  $settings = Get-V3Settings
  $body = @{
    autoStart = $Enabled
    expectedRevision = [uint64]$settings.revision
    processGeneration = [uint64]$settings.processGeneration
  } | ConvertTo-Json
  Invoke-RestMethod `
    http://127.0.0.1:9042/dashboard/api/v3/settings `
    -Method Put `
    -ContentType 'application/json' `
    -Body $body | Out-Null
}

function Stop-InstalledGui {
  param([string]$ExecutablePath)
  if (!$ExecutablePath) { return }
  Get-Process -Name ocg-manager -ErrorAction SilentlyContinue | Where-Object {
    try { $_.Path -eq $ExecutablePath } catch { $false }
  } | ForEach-Object {
    Stop-Process -Id $_.Id -Force
    if (!$_.WaitForExit(30000)) { throw "GUI process $($_.Id) did not stop" }
  }
}

function Invoke-Installer {
  param(
    [string]$Path,
    [string[]]$Arguments,
    [string]$Label,
    [int]$TimeoutSeconds = 180
  )
  Write-Host "Starting $Label"
  # Wait only for the launched installer process. The updater /R path leaves a
  # restarted GUI running, and the uninstaller may hand work to a temporary
  # child. Their observable postconditions are checked separately below.
  $installerProcess = Start-Process $Path -ArgumentList $Arguments -PassThru -WindowStyle Hidden
  if (!$installerProcess.WaitForExit(1000 * $TimeoutSeconds)) {
    try {
      $installerProcess.Kill($true)
      $null = $installerProcess.WaitForExit(30000)
    } catch {
      Stop-Process -Id $installerProcess.Id -Force -ErrorAction SilentlyContinue
    }
    throw "$Label did not exit within $TimeoutSeconds seconds"
  }
  if ($installerProcess.ExitCode -ne 0) {
    throw "$Label failed: $($installerProcess.ExitCode)"
  }
  Write-Host "Completed $Label"
}

$CurrentRunValue = 'Open Console Gateway'
$LegacyRunValue = 'OCG Manager'

function Test-RegistryValue {
  param(
    [string]$Path,
    [string]$Name
  )
  try {
    $key = Get-Item -LiteralPath $Path -ErrorAction Stop
  } catch [System.Management.Automation.ItemNotFoundException] {
    return $false
  }
  return [bool]($key -and ($key.GetValueNames() -contains $Name))
}

function Get-StartupEntryName {
  param([string]$RunKey)
  foreach ($name in @($CurrentRunValue, $LegacyRunValue)) {
    if (Test-RegistryValue -Path $RunKey -Name $name) { return $name }
  }
  return $null
}

function Get-StartupEntryValue {
  param(
    [string]$RunKey,
    [string]$Name
  )
  return (Get-ItemProperty -LiteralPath $RunKey -Name $Name).$Name
}

function Wait-UninstallComplete {
  param(
    [string]$ExecutablePath,
    [string]$UninstallerPath,
    [string]$RunKey,
    [int]$Attempts = 90
  )
  foreach ($attempt in 1..$Attempts) {
    $startupEntryPresent = [bool](Get-StartupEntryName -RunKey $RunKey)
    if (
      !(Test-Path -LiteralPath $ExecutablePath) -and
      !(Test-Path -LiteralPath $UninstallerPath) -and
      !$startupEntryPresent
    ) {
      return
    }
    Start-Sleep 1
  }
  throw 'Candidate uninstall did not remove installed binaries within the timeout'
}

$installDir = [IO.Path]::GetFullPath($InstallDirectory)
$data = [IO.Path]::GetFullPath($DataDirectory)
if (Test-Path -LiteralPath $data) { throw "Hosted runner profile is not clean: $data" }
$sentinel = Join-Path $data preserve-me
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$bootstrapOverwrite = [bool]$PreviousInstaller
$sentinelValue = if ($PreviousTag) { "preserved-from-$PreviousTag" } else { 'preserved' }

$process = $null
$guiPath = ''
try {
  if ($bootstrapOverwrite) {
    $label = if ($PreviousTag) { "published $PreviousTag install" } else { 'previous release install' }
    Invoke-Installer -Path $PreviousInstaller -Arguments @('/S', "/D=$installDir") -Label $label
    $gui = Get-ChildItem $installDir -Recurse -Filter ocg-manager.exe | Select-Object -First 1
    if (!$gui) { throw 'Published GUI executable is missing' }
    $guiPath = $gui.FullName
    $process = Start-Process $guiPath -ArgumentList '--startup' -PassThru -WindowStyle Hidden
    Wait-Dashboard

    New-Item -ItemType Directory -Force $data | Out-Null
    Set-Content $sentinel $sentinelValue
    Set-V3AutoStart -Enabled $true
    $expectedStartupValue = "`"$guiPath`" --startup"
    $previousRunName = Get-StartupEntryName -RunKey $runKey
    if (!$previousRunName) { throw 'Published install did not write a startup entry' }
    $startupValue = Get-StartupEntryValue -RunKey $runKey -Name $previousRunName
    if ($startupValue -ne $expectedStartupValue) { throw "Published install wrote unexpected startup value: $startupValue" }

    $legacyShortcutDirectories = @()
    if ($previousRunName -eq $LegacyRunValue) {
      $legacyShortcutDirectories = @(
        [Environment]::GetFolderPath('Programs'),
        [Environment]::GetFolderPath('DesktopDirectory')
      ) | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'OCG Manager.lnk') }
      if (!$legacyShortcutDirectories.Count) { throw 'Published install has no legacy shortcut to verify' }
    }

    $previousPid = $process.Id
    Invoke-Installer -Path $CandidateInstaller -Arguments @('/UPDATE', '/P', '/R', '/ARGS', '--startup') -Label 'candidate overwrite update'
    foreach ($attempt in 1..30) {
      if (!(Get-Process -Id $previousPid -ErrorAction SilentlyContinue)) { break }
      Start-Sleep 1
    }
    if (Get-Process -Id $previousPid -ErrorAction SilentlyContinue) {
      throw "Published GUI process $previousPid survived the overwrite update"
    }

    if (!(Test-Path -LiteralPath $guiPath)) {
      throw 'Overwrite update moved the GUI out of the existing installation directory'
    }
    $defaultInstallDir = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Open Console Gateway'))
    if ($installDir -ne $defaultInstallDir) {
      $secondCopy = Join-Path $defaultInstallDir 'ocg-manager.exe'
      if (Test-Path -LiteralPath $secondCopy) {
        throw 'Overwrite update installed a second copy under the renamed default directory'
      }
    }

    $updateStatus = Wait-Dashboard -ExpectedVersion $CandidateVersion -Attempts 90
    if ($updateStatus.currentVersion -ne $CandidateVersion) {
      throw "Unexpected updated GUI version: $($updateStatus.currentVersion)"
    }
    $updatedSettings = Get-V3Settings
    if (!$updatedSettings.autoStart) { throw 'Overwrite update did not preserve the auto-start setting' }
    if ((Get-Content $sentinel -Raw).Trim() -ne $sentinelValue) {
      throw 'Overwrite update did not preserve the data sentinel'
    }
    $updatedRunName = Get-StartupEntryName -RunKey $runKey
    if (!$updatedRunName) { throw 'Overwrite update removed the startup entry' }
    $startupValue = Get-StartupEntryValue -RunKey $runKey -Name $updatedRunName
    if ($startupValue -ne $expectedStartupValue) { throw "Overwrite update changed startup value: $startupValue" }
    if ($previousRunName -eq $LegacyRunValue) {
      if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\OCG Manager') {
        throw 'Overwrite update left the legacy installed-app registration behind'
      }
      if (Test-Path 'HKCU:\Software\klarkxy\OCG Manager') {
        throw 'Overwrite update left the legacy installation location behind'
      }
      $shortcutShell = New-Object -ComObject WScript.Shell
      try {
        foreach ($shortcutDirectory in $legacyShortcutDirectories) {
          $currentShortcut = Join-Path $shortcutDirectory 'Open Console Gateway.lnk'
          if (!(Test-Path -LiteralPath $currentShortcut)) {
            throw "Overwrite update did not preserve shortcut: $currentShortcut"
          }
          $shortcut = $shortcutShell.CreateShortcut($currentShortcut)
          try {
            if ($shortcut.TargetPath -ne $guiPath) { throw "Updated shortcut has the wrong target: $currentShortcut" }
          } finally {
            [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) | Out-Null
          }
          if (Test-Path -LiteralPath (Join-Path $shortcutDirectory 'OCG Manager.lnk')) {
            throw 'Overwrite update left a duplicate legacy shortcut'
          }
        }
      } finally {
        [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcutShell) | Out-Null
      }
    }
  } else {
    Invoke-Installer -Path $CandidateInstaller -Arguments @('/S', "/D=$installDir") -Label 'candidate install'
    $gui = Get-ChildItem $installDir -Recurse -Filter ocg-manager.exe | Select-Object -First 1
    if (!$gui) { throw 'Installed GUI executable is missing' }
    $guiPath = $gui.FullName
    $process = Start-Process $guiPath -ArgumentList '--startup' -PassThru -WindowStyle Hidden
    Wait-Dashboard -ExpectedVersion $CandidateVersion | Out-Null
    New-Item -ItemType Directory -Force $data | Out-Null
    Set-Content $sentinel $sentinelValue
  }

  Set-V3AutoStart -Enabled $true
  $startupValue = Get-StartupEntryValue -RunKey $runKey -Name $CurrentRunValue
  $expectedStartupValue = "`"$guiPath`" --startup"
  if ($startupValue -ne $expectedStartupValue) { throw "Unexpected startup value: $startupValue" }
  if (Test-RegistryValue -Path $runKey -Name $LegacyRunValue) {
    throw 'Enabling auto-start left the legacy startup entry behind'
  }

  Set-V3AutoStart -Enabled $false
  if (Get-StartupEntryName -RunKey $runKey) {
    throw 'Disabling auto-start left the startup entry behind'
  }
  Set-V3AutoStart -Enabled $true
  $startupValue = Get-StartupEntryValue -RunKey $runKey -Name $CurrentRunValue
  if ($startupValue -ne $expectedStartupValue) { throw "Unexpected restored startup value: $startupValue" }
} finally {
  if ($process -and !$process.HasExited) {
    Stop-Process -Id $process.Id -Force
    if (!$process.WaitForExit(30000)) { throw "GUI process $($process.Id) did not stop" }
  }
  Stop-InstalledGui $guiPath
}

$uninstaller = Get-ChildItem $installDir -Recurse -Filter uninstall.exe | Select-Object -First 1
if (!$uninstaller) { throw 'Uninstaller is missing' }
Invoke-Installer -Path $uninstaller.FullName -Arguments @('/S') -Label 'candidate uninstall'
Wait-UninstallComplete -ExecutablePath $guiPath -UninstallerPath $uninstaller.FullName -RunKey $runKey
if (Get-StartupEntryName -RunKey $runKey) {
  throw 'Uninstall left the startup entry behind'
}
if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Open Console Gateway') {
  throw 'Uninstall left the installed-app registration behind'
}
if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\OCG Manager') {
  throw 'Uninstall left the legacy installed-app registration behind'
}
if (Test-Path 'HKCU:\Software\klarkxy\OCG Manager') {
  throw 'Uninstall left the legacy installation location behind'
}
if (!(Test-Path $sentinel)) { throw 'Silent uninstall deleted user data' }

Invoke-Installer -Path $CandidateInstaller -Arguments @('/S', "/D=$installDir") -Label 'candidate reinstall'
$gui = Get-ChildItem $installDir -Recurse -Filter ocg-manager.exe | Select-Object -First 1
if (!$gui) { throw 'Reinstalled GUI executable is missing' }
$guiPath = $gui.FullName
$process = Start-Process $guiPath -ArgumentList '--startup' -PassThru -WindowStyle Hidden
try {
  Wait-Dashboard -ExpectedVersion $CandidateVersion | Out-Null
  if ((Get-Content $sentinel -Raw).Trim() -ne $sentinelValue) {
    throw 'Reinstall after silent uninstall lost the data sentinel'
  }
} finally {
  if ($process -and !$process.HasExited) {
    Stop-Process -Id $process.Id -Force
    if (!$process.WaitForExit(30000)) { throw "Reinstalled GUI process $($process.Id) did not stop" }
  }
  Stop-InstalledGui $guiPath
}

$uninstaller = Get-ChildItem $installDir -Recurse -Filter uninstall.exe | Select-Object -First 1
if (!$uninstaller) { throw 'Reinstall uninstaller is missing' }
Invoke-Installer -Path $uninstaller.FullName -Arguments @('/S') -Label 'candidate reinstall uninstall'
Wait-UninstallComplete -ExecutablePath $guiPath -UninstallerPath $uninstaller.FullName -RunKey $runKey
if (Get-StartupEntryName -RunKey $runKey) {
  throw 'Reinstall uninstall left the startup entry behind'
}
if (!(Test-Path $sentinel)) { throw 'Second silent uninstall deleted user data' }

Write-Host "Windows release smoke passed for v$CandidateVersion."
