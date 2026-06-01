<#
  Neuron Auto-Refactor — Windows Görev Zamanlayıcı kaydı.
  Varsayılan: tek seferlik 02:00 (kuruluş turu) + her gün 14:00 ve 21:00.

  ÖNEMLİ: Bunu KENDİ (sandbox'sız) PowerShell pencerende çalıştır:
    powershell -ExecutionPolicy Bypass -File tasks\auto-refactor\install-task.ps1

  Saatleri değiştir (örnekler):
    ... install-task.ps1 -DailyAt 03:00                  # sadece gece 03:00
    ... install-task.ps1 -DailyAt 09:00,17:00,23:00      # günde 3
    ... install-task.ps1 -OnceAt '' -DailyAt 14:00,21:00 # tek-seferlik tur olmadan

  Kaldır / durdur / başlat / hemen çalıştır:
    Unregister-ScheduledTask -TaskName 'Neuron Auto-Refactor' -Confirm:$false
    Disable-ScheduledTask -TaskName 'Neuron Auto-Refactor'
    Enable-ScheduledTask  -TaskName 'Neuron Auto-Refactor'
    Start-ScheduledTask   -TaskName 'Neuron Auto-Refactor'
#>
param(
  [string]$TaskName = 'Neuron Auto-Refactor',
  [string]$OnceAt = '02:00',                    # tek seferlik kurulus turu ('' = atla)
  [string[]]$DailyAt = @('14:00','21:00'),      # her gun bu saatlerde tekrar
  [switch]$Deep,                                # buyuk Tier-3 refactor gorevi (run.ps1 -Deep, uzun pencere)
  [switch]$AutoApply                            # READY patch'leri canli tree'ye onaysiz uygula (run.ps1 -AutoApply)
)

$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'run.ps1'
$repo   = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not (Test-Path $script)) { throw "run.ps1 bulunamadi: $script" }

# Deep gorev: ayri isim + 'run.ps1 -Deep' + uzun ExecutionTimeLimit (cold cargo build + 120dk ajan)
$argExtra = ''
$limitH = 2
if ($Deep) {
  # ALL-deep: ayni isimli ana gorevi -Force ile deep'e cevirir (ayri 'DEEP' gorev YOK).
  $argExtra = ' -Deep'
  $limitH = 4
}
if ($AutoApply) { $argExtra += ' -AutoApply' }

$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
  -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$script`"$argExtra" `
  -WorkingDirectory $repo

# --- Trigger'lar: 1 tek-seferlik + N gunluk ---------------------------------
$triggers = @()
if ($OnceAt) {
  $t0 = [DateTime]::Today.Add([TimeSpan]::Parse($OnceAt))   # bugun OnceAt
  if ($t0 -lt (Get-Date)) { $t0 = $t0.AddDays(1) }          # gectiyse yarin
  $triggers += New-ScheduledTaskTrigger -Once -At $t0
}
foreach ($d in $DailyAt) {
  $triggers += New-ScheduledTaskTrigger -Daily -At $d
}
if (-not $triggers.Count) { throw "En az bir trigger gerekli (OnceAt veya DailyAt)." }

$settings = New-ScheduledTaskSettingsSet `
  -StartWhenAvailable `
  -MultipleInstances IgnoreNew `
  -ExecutionTimeLimit (New-TimeSpan -Hours $limitH) `
  -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries

Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $triggers `
  -Settings $settings -User $env:USERNAME `
  -Description 'Neuron otonom iyilestirme turu — izole worktree, 6 kapi, patch onerisi. Bkz tasks/auto-refactor/PLAYBOOK.md' `
  -Force | Out-Null

Write-Host "OK Kaydedildi: '$TaskName'"
if ($Deep) { Write-Host "   MOD: DEEP (buyuk Tier-3 refactor; ${limitH}h limit; run.ps1 -Deep)" }
if ($OnceAt) { Write-Host ("   Tek seferlik kurulus turu : {0:yyyy-MM-dd HH:mm}" -f $t0) }
Write-Host   ("   Her gun                   : {0}" -f ($DailyAt -join ', '))
Write-Host   "   Script: $script"
Write-Host ""
Write-Host "Sonraki calisma zamani:"
(Get-ScheduledTask -TaskName $TaskName | Get-ScheduledTaskInfo).NextRunTime
Write-Host ""
Write-Host "Tokensiz kuru test:  powershell -ExecutionPolicy Bypass -File `"$script`" -DryRun -SkipGates"
