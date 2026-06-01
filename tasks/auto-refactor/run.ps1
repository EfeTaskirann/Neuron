<#
  Neuron — Otonom İyileştirme Runner
  ----------------------------------
  Canlı working tree'ye SIFIR yan etkiyle bir iyileştirme turu çalıştırır:
    kilit -> snapshot (temp index) -> izole worktree -> claude -p -> kapilar
    -> proposals/<stamp>.patch + log/<stamp>.md -> temizlik.

  Bkz. tasks/auto-refactor/PLAYBOOK.md (§1 akış, §7 kapılar, §8 sınırlar).

  Kullanım:
    powershell -ExecutionPolicy Bypass -File tasks\auto-refactor\run.ps1
    ... run.ps1 -DryRun -SkipGates   # claude YOK, kapi YOK; sadece plumbing
    ... run.ps1 -Focus ui            # rotasyonu ez
    ... run.ps1 -Mode auto-commit
#>
[CmdletBinding()]
param(
  [ValidateSet('proposal','auto-commit')] [string]$Mode = 'proposal',
  [string]$Focus = '',                 # '' = rotasyon; yoksa: health|ui|fe|be|debt
  [int]$TimeoutMin = 45,               # claude ajaninin azami suresi
  [int]$CargoTimeoutMin = 20,          # cargo check/test kapilarinin azami suresi
  [double]$BudgetUsd = 0,              # 0 = cap YOK. Abonelikte nosyonel maliyet cap'i ajani tool ortasinda kesiyor; gercek sinir wall-clock timeout.
  [string]$Model = 'opus',
  [string]$Effort = '',                # '' | low|medium|high|xhigh|max — buyuk refactor icin 'high'/'max'
  [switch]$Deep,                       # buyuk Tier-3 refactor turu: uzun pencere + yuksek effort + satir siniri yok
  [switch]$AutoApply,                  # READY ise patch'i canli working tree'ye ONAYSIZ uygula (zincirleme ilerleme)
  [switch]$DryRun,
  [switch]$SkipGates
)

# --- Deep mode varsayilanlari (buyuk, uzun refactor turu) -------------------
if ($Deep) {
  if ($TimeoutMin -eq 45) { $TimeoutMin = 120 }                  # buyuk refactor icin uzun pencere
  if ([string]::IsNullOrEmpty($Effort)) { $Effort = 'high' }     # daha cok dusunme/calisma
  if ([string]::IsNullOrEmpty($Focus))  { $Focus  = 'deep' }     # rotasyonu deep hedefine sabitle
}

# --- Yollar -----------------------------------------------------------------
$ErrorActionPreference = 'Continue'   # native git/cargo stderr'i HATA sanma (PS5.1 tuzağı)
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}  # native cikti UTF-8 decode (mojibake'i azaltir)
$base     = $PSScriptRoot                                   # tasks/auto-refactor
$repo     = (Resolve-Path (Join-Path $base '..\..')).Path   # repo kökü
$autoBase = Join-Path $env:USERPROFILE '.neuron-auto'
$wtRoot   = Join-Path $autoBase 'wt'
$lockFile = Join-Path $autoBase 'run.lock'
$counterF = Join-Path $autoBase 'counter.txt'
$env:CARGO_TARGET_DIR = Join-Path $autoBase 'cargo-target'  # canlı target/ ile çakışma yok
$env:Path += ";$env:USERPROFILE\.cargo\bin"                 # cargo'yu garanti et
New-Item -ItemType Directory -Force -Path $autoBase,$wtRoot,"$base\log","$base\proposals" | Out-Null

$stamp     = Get-Date -Format 'yyyy-MM-dd_HHmm'
$engineLog = Join-Path $base "log\$stamp.engine.log"
$agentOut  = Join-Path $base "log\$stamp.agent.out.log"
$agentErr  = Join-Path $base "log\$stamp.agent.err.log"
$gateLog   = Join-Path $base "log\$stamp.gates.log"
$wt        = Join-Path $wtRoot $stamp

# --- Yardımcılar -------------------------------------------------------------
function Say([string]$m) { Write-Host "[$(Get-Date -Format HH:mm:ss)] $m" }

# Hizli, asilmayan komutlar (pnpm/git): cikti log'a, exit kodu doner.
function Invoke-Native([string]$exe, [string[]]$argv, [string]$logf) {
  ("`n===== {0} {1} =====" -f $exe, ($argv -join ' ')) | Out-File $logf -Append -Encoding utf8
  & $exe @argv 2>&1 | Tee-Object -FilePath $logf -Append | Out-Null
  return $LASTEXITCODE
}

# cargo: timeout'lu, PID-agaci taskkill ile (asilan testler turu kilitlemesin).
function Invoke-CargoTimed([string[]]$argv, [string]$logf, [int]$timeoutSec) {
  ("`n===== cargo {0} (timeout {1}s) =====" -f ($argv -join ' '), $timeoutSec) | Out-File $logf -Append -Encoding utf8
  $exe = (Get-Command cargo -ErrorAction SilentlyContinue).Source; if (-not $exe) { $exe = 'cargo' }
  $o = New-TemporaryFile; $e = New-TemporaryFile
  $p = Start-Process -FilePath $exe -ArgumentList $argv -WorkingDirectory (Get-Location).Path `
        -NoNewWindow -PassThru -RedirectStandardOutput $o.FullName -RedirectStandardError $e.FullName
  $done = $p.WaitForExit($timeoutSec * 1000)
  if (-not $done) {
    & taskkill /T /F /PID $p.Id 2>&1 | Out-Null
    ("TIMEOUT {0}s - cargo agaci olduruldu (PID {1})" -f $timeoutSec, $p.Id) | Out-File $logf -Append -Encoding utf8
    $code = 124
  } else { $code = $p.ExitCode }
  Get-Content $o.FullName, $e.FullName -ErrorAction SilentlyContinue | Out-File $logf -Append -Encoding utf8
  Remove-Item $o.FullName, $e.FullName -Force -ErrorAction SilentlyContinue
  return $code
}

# --- Kilit (üst üste binme yok) ---------------------------------------------
if (Test-Path $lockFile) {
  $lpid = ((Get-Content $lockFile -Raw -ErrorAction SilentlyContinue) -as [string]).Trim() -as [int]
  $alive = if ($lpid) { [bool](Get-Process -Id $lpid -ErrorAction SilentlyContinue) } else { $false }
  if ($alive) { Write-Host "Baska bir tur aktif (PID $lpid). Cikiliyor."; exit 0 }
  Remove-Item $lockFile -Force -ErrorAction SilentlyContinue   # stale
}
"$PID" | Set-Content $lockFile -Encoding ascii

Start-Transcript -Path $engineLog -Append | Out-Null
$snap = $null; $verdict = 'UNKNOWN'; $timedOut = $false
try {
  Say "Repo: $repo"
  Say "Mod: $Mode | Deep: $Deep | timeout: ${TimeoutMin}dk | Model: $Model | Effort: $(if($Effort){$Effort}else{'(default)'}) | DryRun: $DryRun"

  # --- Rotasyon ekseni ------------------------------------------------------
  $axisNames = @('health','ui','fe','be','debt')
  $axisLabel = @{ health='Tab fonksiyonel saglik'; ui='UI / UX'; fe='Frontend refactor'; be='Backend refactor (stable)'; debt='Borc / docs / test'; deep='DEEP - buyuk Tier-3 refactor' }
  if ($Focus) {
    $axis = $Focus
  } else {
    $c = if (Test-Path $counterF) { (Get-Content $counterF -Raw).Trim() -as [int] } else { 0 }
    if ($null -eq $c) { $c = 0 }
    $axis = $axisNames[$c % $axisNames.Count]
    ($c + 1) | Set-Content $counterF -Encoding ascii
  }
  Say "Rotasyon ekseni: $axis ($($axisLabel[$axis]))"

  # --- Snapshot: tam working tree, ayri index, SIFIR yan etki ---------------
  $head = (git -C $repo rev-parse HEAD).Trim()
  $tmpIdx = Join-Path $env:TEMP "neuron-snap-$stamp.idx"
  if (Test-Path $tmpIdx) { Remove-Item $tmpIdx -Force }
  $env:GIT_INDEX_FILE = $tmpIdx
  git -C $repo read-tree HEAD | Out-Null
  git -C $repo add -A | Out-Null
  $tree = (git -C $repo write-tree).Trim()
  Remove-Item Env:\GIT_INDEX_FILE
  Remove-Item $tmpIdx -Force -ErrorAction SilentlyContinue
  $snap = (git -C $repo commit-tree $tree -p $head -m "auto-refactor snapshot $stamp").Trim()
  Say "Snapshot: $snap (base HEAD $head)"

  # --- Hot-zone (dirty) listesi ---------------------------------------------
  $dirty = @(git -C $repo status --porcelain | ForEach-Object { ($_ -replace '^.{3}', '').Trim('"') })
  Say "Hot-zone dirty dosya: $($dirty.Count)"

  # --- Izole worktree -------------------------------------------------------
  git -C $repo worktree add --detach $wt $snap | Out-Null
  if ($LASTEXITCODE -ne 0 -or -not (Test-Path $wt)) { throw "worktree add basarisiz (exit $LASTEXITCODE)" }
  Say "Worktree: $wt"

  # Otonom-mod CLAUDE.md (canli repo'nunki degismez)
  Copy-Item (Join-Path $base 'autonomous-CLAUDE.md') (Join-Path $wt 'CLAUDE.md') -Force

  # --- .run-context.md (worktree'ye) ----------------------------------------
  $lastSummary = if (Test-Path (Join-Path $base 'LATEST.md')) { (Get-Content (Join-Path $base 'LATEST.md') -Raw) } else { '(ilk tur)' }
  $dirtyBlock  = if ($dirty.Count) { ($dirty | ForEach-Object { "- $_" }) -join "`n" } else { '(temiz - hepsi APPLY edilebilir)' }
  $ctx = @(
    "# Run context - $stamp", "",
    "- RUN_STAMP: $stamp",
    "- Rotasyon ekseni: **$axis** - $($axisLabel[$axis])",
    "- Mod: $Mode",
    "- Snapshot: $snap (base HEAD $head)", "",
    "## Hot-zone (dirty, AUDIT-only) dosyalar",
    $dirtyBlock, "",
    "## Onceki turun ozeti",
    $lastSummary
  )
  if ($Deep) {
    $ctx += @(
      "", "## [DEEP MODE] - BUYUK REFACTOR TURU (bu blok kapsam/satir sinirlarini EZER)",
      "Bu tur KUCUK degil. BACKLOG Tier-3'ten tek bir buyuk, STABLE (dirty olmayan) hedef sec:",
      "commands/swarm.rs (2878L) / swarm/projector.rs (2430L) / swarm/brain.rs (2013L) / swarm/agent_dispatcher.rs (1967L) / sidecar/terminal.rs (2049L).",
      "(parcali-PR/audit isaretli olsalar bile DEEP modda APPLY edilebilir - yeter ki dirty/hot-zone olmasin).",
      "- Refactor'i bu turda TAM bitir: alt-modullere bol, kodu tasi, TUM import/referanslari duzelt, davranisi BIREBIR koru.",
      "- Satir siniri YOK - 1000+ satirlik diff beklenir ve iyidir. Yarim birakma, erken durma, audit'e kacma.",
      "- Zorunlu oz-kontrol: cargo check --manifest-path src-tauri/Cargo.toml YESIL kalmali; collect_commands! ve public API yuzeyi aynen korunmali.",
      "- Hot-zone (dirty W6) dosyalara yine DOKUNMA; hedef dirty ise baska stable buyuk hedef sec.",
      "- Bittiginde BACKLOG'da maddeyi isaretle; neyi nereye bolduguni raporda ozetle."
    )
  }
  Set-Content -Path (Join-Path $wt 'tasks\auto-refactor\.run-context.md') -Value $ctx -Encoding utf8

  # --- Ajan (claude -p) — launcher .ps1 + timeout'lu PID-agaci kill ----------
  if ($DryRun) {
    Say "DryRun: claude atlaniyor."
  } else {
    Say "claude -p baslatiliyor (launcher.ps1, cikti stream-json log'a)..."
    $budgetArg = if ($BudgetUsd -gt 0) { "--max-budget-usd $BudgetUsd" } else { '' }
    $effortArg = if ($Effort) { "--effort $Effort" } else { '' }
    $launcher = Join-Path $autoBase 'agent-launcher.ps1'
    $launcherBody = @(
      "`$ErrorActionPreference = 'Continue'",
      "Set-Location -LiteralPath '$wt'",
      "`$p = Get-Content 'tasks/auto-refactor/PROMPT.md' -Raw",
      "`$p | & claude -p --permission-mode acceptEdits --allowedTools 'Bash Edit Write' --add-dir '$wt' --model $Model $effortArg --output-format stream-json --verbose $budgetArg",
      "exit `$LASTEXITCODE"
    )
    Set-Content -Path $launcher -Value $launcherBody -Encoding ascii
    $proc = Start-Process -FilePath 'powershell.exe' `
      -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File', $launcher) `
      -WorkingDirectory $wt -NoNewWindow -PassThru `
      -RedirectStandardOutput $agentOut -RedirectStandardError $agentErr
    if (-not $proc.WaitForExit($TimeoutMin * 60 * 1000)) {
      Say "TIMEOUT (${TimeoutMin}dk) - surec agaci olduruluyor (PID $($proc.Id))."
      & taskkill /T /F /PID $proc.Id 2>&1 | Out-Null
      $timedOut = $true
    } else {
      Say "claude bitti (exit $($proc.ExitCode))."
    }
  }

  # --- Degisen dosyalar (gate'leri scope'lamak icin, gate'ten ONCE) ----------
  $pathspec = @('--','.', ':(exclude)CLAUDE.md', ':(exclude)tasks/auto-refactor/log', ':(exclude)tasks/auto-refactor/.run-context.md')
  git -C $wt add -A | Out-Null
  $changed = @(& git -C $wt diff --cached --name-only $snap @pathspec)
  $touchedRust  = [bool]($changed | Where-Object { $_ -like 'src-tauri/*' })
  $touchedFront = [bool]($changed | Where-Object { $_ -like 'app/*' })
  Say ("Degisen dosya: {0} (rust={1} front={2})" -f $changed.Count, $touchedRust, $touchedFront)

  # --- Kapilar (degisene gore scope + cargo'da timeout) ----------------------
  $gates = [ordered]@{}
  $cargoSec = $CargoTimeoutMin * 60
  if ($SkipGates) {
    Say "SkipGates: kapilar atlandi."
  } elseif ($changed.Count -eq 0) {
    Say "Degisiklik yok - kod kapisi gereksiz."
  } else {
    Push-Location $wt
    try {
      Say "pnpm install..."
      $instOk = (Invoke-Native 'pnpm' @('install','--frozen-lockfile') $gateLog) -eq 0
      if (-not $instOk) {
        Say "pnpm install BASARISIZ - kapilar kosturulamiyor."
      } else {
        if ($touchedRust) {
          Say "cargo check (timeout ${CargoTimeoutMin}dk)..."
          $gates['cargo check']        = (Invoke-CargoTimed @('check','--manifest-path','src-tauri/Cargo.toml') $gateLog $cargoSec) -eq 0
          Say "cargo test (timeout ${CargoTimeoutMin}dk)..."
          $gates['cargo test']         = (Invoke-CargoTimed @('test','--manifest-path','src-tauri/Cargo.toml') $gateLog $cargoSec) -eq 0
          $gates['gen:bindings:check'] = (Invoke-Native 'pnpm' @('gen:bindings:check') $gateLog) -eq 0
        }
        if ($touchedFront) {
          $gates['typecheck'] = (Invoke-Native 'pnpm' @('--filter','@neuron/app','typecheck') $gateLog) -eq 0
          $gates['lint']      = (Invoke-Native 'pnpm' @('--filter','@neuron/app','lint') $gateLog) -eq 0
          $gates['vitest']    = (Invoke-Native 'pnpm' @('--filter','@neuron/app','exec','vitest','run') $gateLog) -eq 0
        }
      }
    } finally { Pop-Location }
  }
  # --- Baseline-diff: FAIL'ler snapshot'ta ZATEN var miydi? (W6 dirty tree gercegi) ---
  # Sadece FAIL olan kapilari pristine bir baseline worktree'de tekrar kosturup
  # "regresyon mu, pre-existing mi" ayirt ederiz. Pre-existing = ajanin sucu degil.
  $failed = @($gates.GetEnumerator() | Where-Object { -not $_.Value } | ForEach-Object { $_.Key })
  $preExisting = @()
  if ($failed.Count -gt 0 -and -not $SkipGates) {
    Say "FAIL kapilar ($($failed -join ', ')) baseline'da da var mi? kontrol..."
    $wtBase = Join-Path $wtRoot "$stamp-base"
    git -C $repo worktree add --detach $wtBase $snap | Out-Null
    if (Test-Path $wtBase) {
      Push-Location $wtBase
      try {
        Invoke-Native 'pnpm' @('install','--frozen-lockfile') $gateLog | Out-Null
        foreach ($g in $failed) {
          $code = switch ($g) {
            'cargo check'        { Invoke-CargoTimed @('check','--manifest-path','src-tauri/Cargo.toml') $gateLog $cargoSec }
            'cargo test'         { Invoke-CargoTimed @('test','--manifest-path','src-tauri/Cargo.toml') $gateLog $cargoSec }
            'gen:bindings:check' { Invoke-Native 'pnpm' @('gen:bindings:check') $gateLog }
            'typecheck'          { Invoke-Native 'pnpm' @('--filter','@neuron/app','typecheck') $gateLog }
            'lint'               { Invoke-Native 'pnpm' @('--filter','@neuron/app','lint') $gateLog }
            'vitest'             { Invoke-Native 'pnpm' @('--filter','@neuron/app','exec','vitest','run') $gateLog }
            default { 0 }
          }
          if ($code -ne 0) { $preExisting += $g }   # baseline'da da FAIL => pre-existing
        }
      } finally { Pop-Location }
      git -C $repo worktree remove --force $wtBase | Out-Null
      git -C $repo worktree prune | Out-Null
      if (Test-Path $wtBase) { Remove-Item $wtBase -Recurse -Force -ErrorAction SilentlyContinue }
    }
    Say "Pre-existing (ajan degil) FAIL: $(if($preExisting.Count){$preExisting -join ', '}else{'yok'})"
  }
  # Regresyon = FAIL ama baseline'da PASS olan kapilar. Verdict yalniz regresyona bakar.
  $regressions = @($failed | Where-Object { $preExisting -notcontains $_ })
  $allGreen = if ($gates.Count -gt 0) { $regressions.Count -eq 0 } else { -not ($touchedRust -or $touchedFront) }

  # --- Patch (ajanin degisiklik seti, snapshot'a karsi) ----------------------
  # git'in KENDISI dosyaya yazar (--output): PowerShell string round-trip'i UTF-8'i bozup
  # "corrupt patch" yapiyordu (ellipsis/Turkce). --output exact byte verir, git apply temiz calisir.
  git -C $wt add -A | Out-Null
  $shortstat  = ((& git -C $wt diff --cached $snap --shortstat @pathspec) -join ' ').Trim()
  $patchPath  = Join-Path $base "proposals\$stamp.patch"
  git -C $wt diff --cached $snap --output="$patchPath" @pathspec
  $hasChanges = (Test-Path $patchPath) -and ((Get-Item $patchPath).Length -gt 0)
  if ($hasChanges) { Say "Patch: $patchPath ($shortstat)" }
  else { Remove-Item $patchPath -Force -ErrorAction SilentlyContinue; Say "Kod degisikligi yok (audit/noop)." }

  # --- Verdict --------------------------------------------------------------
  if ($timedOut)            { $verdict = 'TIMEOUT' }
  elseif (-not $hasChanges) { $verdict = 'AUDIT-only / NOOP' }
  elseif ($SkipGates)       { $verdict = 'UNVERIFIED (SkipGates)' }
  elseif ($allGreen)        { $verdict = 'READY' }
  else                      { $verdict = 'REJECTED (kapi kirik)' }
  Say "Verdict: $verdict"

  # --- auto-commit modu -----------------------------------------------------
  if ($Mode -eq 'auto-commit' -and $hasChanges -and $allGreen -and -not $timedOut) {
    $branch = "auto/$stamp"
    git -C $wt checkout -b $branch $snap | Out-Null
    git -C $wt add -A | Out-Null
    $msg = "refactor(auto): tur $stamp ($axis)`n`nOtonom iyilestirme turu. Bkz tasks/auto-refactor/log/$stamp.md"
    git -C $wt -c core.hooksPath=NUL commit -m $msg | Out-Null
    git -C $wt push origin $branch | Out-Null
    Say "auto-commit: dal $branch push edildi (exit $LASTEXITCODE)."
  }

  # --- Auto-apply: READY ise patch'i CANLI working tree'ye onaysiz uygula --------
  # Boylece ilerleme zincirlenir: bir sonraki tur uygulanmis durumu snapshot'lar.
  if ($AutoApply -and $verdict -eq 'READY' -and $hasChanges -and $Mode -ne 'auto-commit') {
    git -C $repo apply --whitespace=nowarn $patchPath
    if ($LASTEXITCODE -eq 0) { Say "AUTO-APPLY: patch canli working tree'ye uygulandi (onaysiz)." }
    else { Say "AUTO-APPLY BASARISIZ (baglam kaymasi/cakisma) - patch proposals'ta kaldi, manuel gerekir." }
  }

  # --- Conditional string parcalari (here-string YOK) ------------------------
  $diffStr  = if ($hasChanges) { $shortstat } else { 'yok' }
  $patchStr = if ($hasChanges) { "proposals/$stamp.patch" } else { '-' }

  # --- Rapor (ajanin raporunu al, runner kapi blogunu ekle) ------------------
  $liveReport = Join-Path $base "log\$stamp.md"
  $wtReport   = Join-Path $wt "tasks\auto-refactor\log\$stamp.md"
  if (Test-Path $wtReport) {
    Copy-Item $wtReport $liveReport -Force
  } else {
    Set-Content $liveReport -Encoding utf8 -Value @("# Otonom tur - $stamp", "", "(Ajan rapor uretmedi. DryRun=$DryRun, Timeout=$timedOut.)")
  }
  $gateTable = ($gates.GetEnumerator() | ForEach-Object {
    $mark = if ($_.Value) { 'PASS' } elseif ($preExisting -contains $_.Key) { 'FAIL (pre-existing - W6, ajan degil)' } else { 'FAIL (REGRESYON - ajan kirdi)' }
    "| $($_.Key) | $mark |"
  }) -join "`n"
  if (-not $gateTable) { $gateTable = "| (kod kapisi calismadi) | - |" }
  $runnerLines = @(
    "", "---", "## Runner dogrulamasi (otorite)",
    "- **Verdict:** $verdict",
    "- **Eksen / Mod:** $axis / $Mode",
    "- **Degisen:** $($changed.Count) dosya (rust=$touchedRust front=$touchedFront)",
    "- **Snapshot:** $snap (base $head) - dirty $($dirty.Count)",
    "- **Diff:** $diffStr",
    "- **Patch:** $patchStr", "",
    "| Kapi | Sonuc |", "|---|---|",
    $gateTable, "",
    "Loglar: log/$stamp.gates.log  log/$stamp.agent.out.log  log/$stamp.engine.log"
  )
  Add-Content $liveReport -Value $runnerLines -Encoding utf8

  # --- LATEST.md + HISTORY.md ----------------------------------------------
  $gatesSummary = if ($gates.Count) {
    ($gates.GetEnumerator() | ForEach-Object { "$($_.Key)=$(if ($_.Value) { 'OK' } else { 'X' })" }) -join ' '
  } else { '(kod kapisi calismadi)' }
  $latestLines = @(
    "# Son otonom tur: $stamp", "",
    "- **Verdict:** $verdict",
    "- **Eksen / Mod:** $axis ($($axisLabel[$axis])) / $Mode",
    "- **Diff:** $diffStr",
    "- **Rapor:** log/$stamp.md   |   **Patch:** $patchStr",
    "- **Kapilar:** $gatesSummary", "",
    "> READY ise patch'i incele:  git apply tasks/auto-refactor/proposals/$stamp.patch  (once --check ile dene)"
  )
  Set-Content (Join-Path $base 'LATEST.md') -Value $latestLines -Encoding utf8

  $histF = Join-Path $base 'HISTORY.md'
  if (-not (Test-Path $histF)) {
    Set-Content $histF -Encoding utf8 -Value @("# Tur gecmisi", "", "| Stamp | Eksen | Mod | Verdict | Diff |", "|---|---|---|---|---|")
  }
  Add-Content $histF -Encoding utf8 -Value ("| {0} | {1} | {2} | {3} | {4} |" -f $stamp, $axis, $Mode, $verdict, $diffStr)

  Say "Tamamlandi: $verdict"
}
catch {
  Say "HATA: $($_.Exception.Message)"
  $verdict = "RUNNER ERROR: $($_.Exception.Message)"
}
finally {
  # --- Temizlik: worktree sil, kilidi birak (cargo-target kalir) ------------
  if ($snap -and (Test-Path $wt)) {
    git -C $repo worktree remove --force $wt | Out-Null
    git -C $repo worktree prune | Out-Null
  }
  if (Test-Path $wt) { Remove-Item $wt -Recurse -Force -ErrorAction SilentlyContinue }
  Remove-Item $lockFile -Force -ErrorAction SilentlyContinue
  Say "Temizlik bitti. Canli working tree'ye dokunulmadi."
  try { Stop-Transcript | Out-Null } catch {}
}
