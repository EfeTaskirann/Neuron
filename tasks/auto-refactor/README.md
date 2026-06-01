# Neuron — Otonom İyileştirme Sistemi

Belirlediğin saatlerde (**her gün 14:00 ve 21:00**, + ilk kuruluş turu 02:00), izole bir kopyada uygulamayı bir adım iyileştiren, **canlı koduna asla dokunmayan** bir rutin. Odak: **UI optimizasyonu · tab fonksiyonel sağlık · kullanıcı kolaylığı (UX)** + refactor/borç temizliği — sırayla tüm hatlarda.

## Nasıl çalışır (özet)

Her tetik `run.ps1`'i çağırır:

1. **Snapshot** — canlı working tree'nin tam durumu (commit edilmemiş + untracked dahil) ayrı bir git index ile bir commit nesnesine alınır. **Senin dosyalarına / index'ine / branch'ine sıfır dokunuş** (doğrulandı).
2. **İzole worktree** — snapshot OneDrive dışında (`%USERPROFILE%\.neuron-auto\wt\`) açılır. Ajan yalnızca orada çalışır.
3. **`claude -p`** headless → PLAYBOOK + BACKLOG'u okur, 1–3 sınırlı madde yapar.
4. **6 kapı** runner tarafından bağımsız doğrulanır (cargo check/test · typecheck · lint · vitest · bindings drift).
5. **Çıktı** → `proposals/<stamp>.patch` (öneri diff) + `log/<stamp>.md` (rapor). `LATEST.md`/`HISTORY.md` güncellenir.
6. **Temizlik** — worktree silinir. (`cargo-target` cache kalır → sonraki turlar hızlı.)

Detaylı kurallar: **[PLAYBOOK.md](PLAYBOOK.md)**. İş listesi: **[BACKLOG.md](BACKLOG.md)**.

## Dosyalar

| Dosya | Ne |
|---|---|
| `PLAYBOOK.md` | Anayasa — ajanın her tur okuduğu kurallar, modlar, kapılar, çıktı formatı |
| `BACKLOG.md` | Önceliklendirilmiş iş listesi (tohum: 9-tab sağlığı, UI/UX, hotspot refactor, borç) |
| `PROMPT.md` | `claude -p`'ye verilen görev metni |
| `autonomous-CLAUDE.md` | Worktree'ye kopyalanan otonom-mod kuralları (canlı `CLAUDE.md` değişmez) |
| `run.ps1` | Motor (snapshot → worktree → claude → kapılar → patch → temizlik) |
| `install-task.ps1` | Windows Görev Zamanlayıcı kaydı |
| `log/` | Tur raporları + ham loglar (git-ignored) |
| `proposals/` | Öneri patch'leri (git-ignored) |
| `LATEST.md` / `HISTORY.md` | Son tur özeti / tüm tur geçmişi |

## Kurulum (3 adım — kendi PowerShell'inde)

```powershell
cd "C:\Users\efeta\OneDrive\Masaüstü\Neuron"

# 1) Tokensiz kuru test — plumbing'i doğrular (snapshot/worktree/temizlik), claude çağırmaz
powershell -ExecutionPolicy Bypass -File tasks\auto-refactor\run.ps1 -DryRun -SkipGates

# 2) (opsiyonel) Tek gerçek tur elle — token harcar, ~10-45 dk
powershell -ExecutionPolicy Bypass -File tasks\auto-refactor\run.ps1

# 3) Zamanlayıcıya kaydet (her gün 14:00 ve 21:00 + ilk sefer 02:00)
powershell -ExecutionPolicy Bypass -File tasks\auto-refactor\install-task.ps1
```

> Görev **sen oturum açıkken** çalışır (PATH'i miras alır; cargo/pnpm/claude bulunur). Bilgisayar o saatte açık ve oturum açık olmalı. Kapalıysa `-StartWhenAvailable` sayesinde açılınca telafi eder.

## Sabah rutini

`LATEST.md`'ye bak. Verdict:

- **✅ READY** → patch'i incele ve uygula:
  ```powershell
  git apply --check tasks\auto-refactor\proposals\<stamp>.patch   # çakışma var mı?
  git apply         tasks\auto-refactor\proposals\<stamp>.patch   # uygula
  ```
- **🟡 AUDIT-only / NOOP** → kod değişmedi; `log/<stamp>.md` içindeki bulguları oku (genelde hot-zone önerileri).
- **❌ REJECTED / TIMEOUT** → değişiklik kapıyı kırdı ya da süre doldu; `log/<stamp>.gates.log` teşhis için duruyor. Uygulanmaz.

## Önemli: hot-zone (şu anki durumun)

Şu an **~17 günlük commit edilmemiş W6 işin** var (`swarm_term/**`, `TerminalSwarm.tsx`, persona'lar...). Bu dosyalar her tur **AUDIT-only**'dir — onlara patch üretilmez (sen üzerinde çalışırken çakışmasın diye). Sistem en çok değeri **stable (commit edilmiş) alanlarda** üretir.

➡️ **Öneri:** W6'yı bir dala commit'le. Commit ettiğin an o alan "stable zone"a düşer ve otomatik iyileştirmeye açılır. Commit etmesen de sistem güvenli — sadece o alanda yorum/öneri yazar, dokunmaz.

## Ayarlar (run.ps1 parametreleri)

| Parametre | Varsayılan | Ne |
|---|---|---|
| `-Mode` | `proposal` | `proposal` (patch) \| `auto-commit` (auto/<stamp> dalına commit+push) |
| `-Focus` | `''` (rotasyon) | `health\|ui\|fe\|be\|debt` — bir ekseni sabitler |
| `-TimeoutMin` | `45` | Tur başına azami süre; aşılırsa süreç ağacı öldürülür |
| `-BudgetUsd` | `0` | $ tavanı. **Abonelikte 0 bırak** — nosyonel maliyet cap'i ajanı tool ortasında keser. Gerçek sınır = `-TimeoutMin`. |
| `-Model` | `opus` | `opus` \| `sonnet` |
| `-DryRun` | — | claude'u atla (plumbing testi) |
| `-SkipGates` | — | kapıları atla (hızlı plumbing testi) |

## Durdurma / değiştirme

```powershell
Disable-ScheduledTask  -TaskName 'Neuron Auto-Refactor'    # duraklat
Enable-ScheduledTask   -TaskName 'Neuron Auto-Refactor'    # devam
Unregister-ScheduledTask -TaskName 'Neuron Auto-Refactor' -Confirm:$false   # tamamen kaldır
Start-ScheduledTask    -TaskName 'Neuron Auto-Refactor'    # hemen bir tur
```
Saatleri değiştir: `install-task.ps1 -DailyAt 03:00` veya `-DailyAt 09:00,17:00` gibi yeniden çalıştır (`-OnceAt ''` ile ilk tek-seferlik turu kapatır).

## Güvenlik garantileri

- Canlı working tree / index / branch'e **yazılmaz** (yalnız izole worktree).
- Kapıyı kıran değişiklik **patch'lenmez** (REJECTED).
- Aynı anda **tek tur** (kilit). Kapsam **tur başına 1–3 madde, ≲400 satır**.
- Charter Hard Constraints (OKLCH, timestamp invariant, wire-shape, no `--no-verify`) korunur.
