# Neuron — Otonom İyileştirme Playbook'u

**Sürüm:** 1.0
**İlk derleme:** 2026-05-31
**Sahip:** Efe Taşkıran
**Tetikleme:** Windows Görev Zamanlayıcı → her gün 14:00 ve 21:00 (+ ilk sefer 02:00), `run.ps1`
**Durum:** Aktif

Bu dosya tek seferlik bir rapor değildir. **Her otonom tur, iş yapmadan önce bu dosyayı baştan sona okur** ve buradaki kurallara göre davranır. [tasks/refactor-v1.md](../refactor-v1.md)'in disiplinini (uncommit tree'ye dokunma, her batch yeşil-gate, somut kanıt) miras alır ve onu yinelenen bir rutine çevirir.

> **Otorite zinciri** (çakışmada üst kazanır): `PROJECT_CHARTER.md` → ilgili `WP-*.md` → `design-system-spec.md` → `NEURON_TERMINAL_REPORT.md` → `AGENTS.md` → **bu Playbook** → ADR'ler → mevcut kod. Bu playbook hiçbir Charter "Hard constraint"ini geçersiz kılamaz.

---

## 1. Sistem nasıl çalışır

Her tur, `run.ps1` tarafından sürülür. Akış:

1. **Kilit** — başka bir tur çalışıyorsa (stale değilse) çık. Üst üste binme yok.
2. **Snapshot** — canlı çalışma ağacının **tam** durumu (tracked + untracked + silmeler dahil) geçici bir git index üzerinden bir commit nesnesine alınır. **Canlı working tree / index / branch'e SIFIR yan etki.** (Doğrulandı: `git read-tree HEAD` + `git add -A` ayrı `GIT_INDEX_FILE` ile → `commit-tree`.)
3. **İzole worktree** — snapshot, OneDrive dışındaki bir yola (`%USERPROFILE%\.neuron-auto\wt\<stamp>`) `git worktree add --detach` ile açılır. Ajan **yalnızca** burada çalışır.
4. **Otonom mod** — worktree'nin `CLAUDE.md`'si `autonomous-CLAUDE.md` ile değiştirilir (canlı repo'nunki değişmez). "Edit'ten önce haber ver" kuralı bu izole, atılabilir bağlamda askıya alınır; tüm Charter/teknik kısıtlar korunur.
5. **Ajan** — `claude -p` headless çalışır (`--permission-mode acceptEdits --allowedTools 'Bash Edit Write'` — Edit/Write/Bash'i sandbox AÇMADAN otomatik kabul eder. Not: `bypassPermissions` headless'ta onaylayıcı olmadığı için reddeder; `--dangerously-skip-permissions` ise git-worktree'de bozuk bir FS-sandbox açıp yazmaları engeller — ikisi de bu bağlamda çalışmadı, doğru olan `acceptEdits`. İzole worktree olduğu için risk yok). Ajan bu playbook + `BACKLOG.md` + `.run-context.md`'yi okur, sınırlı bir iş yapar.
6. **Kapılar (runner doğrular + baseline-diff)** — ajan bittikten sonra runner kapıları **kendi** koşturur (ajanın izlenimine değil, kapılara güven — `AGENTS.md`). FAIL olan kapı, pristine bir baseline worktree'de tekrar koşturulur: yalnız **baseline'da PASS olup ajandan sonra FAIL olan** kapı REGRESYON sayılır (→ REJECTED). Commit edilmemiş W6'dan gelen pre-existing kırıklar ajanın suçu değildir → yine READY.
7. **Çıktı** — runner, ajanın değişiklik setini snapshot'a karşı `proposals/<stamp>.patch` olarak, raporu `log/<stamp>.md` olarak **canlı repo'ya** yazar. `HISTORY.md` ve `LATEST.md` güncellenir.
8. **Temizlik** — worktree silinir, kilit bırakılır. (Cargo target cache `%USERPROFILE%\.neuron-auto\cargo-target` kalır → sonraki turlar incremental.)

### İki mod — working-tree durumuna göre otomatik

- **Apply (uygula):** snapshot anında **temiz / commit edilmiş** dosyalar ("stable zone"). Sınırlı kod değişikliği + 6 kapı + patch üretir.
- **Audit (denetle):** snapshot anında **canlı tree'de dirty olan** dosyalar ("hot zone"). Bunlara patch üretilmez — yalnızca raporda bulgu + öneri yazılır. Sebep: dirty dosyaya üretilen patch, kullanıcı saatler sonra uygularken çakışır.

`.run-context.md`, o turun dirty-dosya listesini ("hot zone") ajana verir. Ajan hot zone'a yalnızca audit yapar.

---

## 2. "Daha iyi"nin tanımı (ölçülebilir)

Bir tur, ancak şunlardan **en az birini** ölçülebilir biçimde iyileştirip **6 kapıyı yeşil** bırakıyorsa başarılıdır. Kullanıcının öncelik eksenleri:

| Eksen | Ne demek | Nasıl ölçülür |
|---|---|---|
| **UI optimizasyonu** | Görsel tutarlılık, design-system parity, ölü CSS, gereksiz re-render | OKLCH ihlali = 0; `design-system-spec.md` sapması azalır; kullanılmayan CSS/komponent silinir; bundle/DOM küçülür |
| **Tab fonksiyonel sağlık** | 9 tab'ın (bkz. §5) hepsi hatasız render olur ve temel etkileşim çalışır | route-render smoke testleri yeşil; her tab için en az 1 "renders without throwing" testi var; ErrorBoundary'ye düşen route = 0 |
| **Kullanıcı kolaylığı (UX)** | Loading/empty/error durumları, klavye kısayolları, toast geri bildirimi, kopyalama/erişilebilirlik | Eksik loading/empty state sayısı azalır; a11y (rol/aria/odak) eksiği azalır; tekrarlı string'ler `lib/copy.ts`'e iner |
| **Refactor / DRY** | Dev dosyaların bölünmesi, tekrarın merkezleşmesi, ölü kodun silinmesi | Hedef dosya satır sayısı düşer; duplike blok sayısı azalır; `cargo clippy` / eslint uyarısı azalır |
| **Performans** | Gereksiz iş, allocation, poll sıklığı, ring-buffer israfı | Ölçülebilir hot-path iyileşmesi (benchmark/test ile gösterilebiliyorsa) |

**Negatif tanım — başarı DEĞİLDİR:** kapıları kıran değişiklik; wire-shape (mock sözleşmesi) sapması; Charter kısıtı ihlali; kapsam bütçesini aşan dev diff; "kozmetik yeniden adlandırma" turu.

---

## 3. Değişmez kurallar (guardrails)

1. **Canlı working tree'ye asla dokunma.** Tüm iş izole worktree'de. Runner bunu zorlar; ajan worktree dışına `--add-dir` almaz.
2. **Charter Hard Constraints korunur** (`PROJECT_CHARTER.md` §"Hard constraints"):
   - Mock wire-shape sözleşmesi (key adları/tipleri) değiştirilemez; tek istisna #1 carve-out (display-derived "now" string'leri).
   - **OKLCH only** — yeni CSS'te hex/HSL yok.
   - **Timestamp invariant** — `_at` = saniye, `_ms` = milisaniye, başka format yok.
   - **No Drizzle / JS ORM** — ORM Rust `sqlx`'te.
   - **Dark-first** — net-new UI karanlık çıkar.
   - **ID stratejisi** ADR-0007 (prefixed-ULID / slug / autoincrement).
   - **`--no-verify` yasak.**
3. **6 kapı zorunlu** (bkz. §7). Biri kırmızıysa o değişiklik patch'lenmez (READY değil, REJECTED etiketiyle teşhise bırakılır).
4. **`bindings.ts` elle düzenlenmez** — `pnpm gen:bindings` ile üretilir; `gen:bindings:check` drift'i yakalar.
5. **Kapsam bütçesi:** tur başına **1–3 ilişkili madde**, toplam diff hedefi **≲ 400 satır**. Büyük mimari değişim (Supervisor trait, MCP session pool, migration squash) **tek turda yapılmaz** — `BACKLOG.md`'de "büyük, parçalı PR gerekir" diye işaretlenir, audit raporu yazılır.
6. **Hot zone = audit-only** (§1). Şu an aktif W6 alanı: `src-tauri/src/swarm_term/**` ve `app/src/routes/TerminalSwarm.tsx` gibi dirty dosyalar. Snapshot'taki dirty listesi `.run-context.md`'den okunur — sabit varsayma, her tur yeniden bak.
7. **Persona dosyaları** (`src-tauri/src/swarm/agents/term/*.md`) davranışsaldır — değiştirmek ajan orkestrasyonunu etkiler. Yalnızca açık, düşük-riskli düzeltme (yazım, çelişki) ve **mutlaka** audit notuyla. Anlamsal değişiklik = audit-only.
8. **Tek WP / tek tema:** bir tur tek bir temaya (bir tab, bir modül) odaklanır. Dağılma yok.
9. **Reference-only alanlar:** `Neuron Design/`, `neuron-docs/` (varsa) — dokunma.
10. **Commit/push yok** (Apply modunda bile). Runner patch üretir; **merge kararı kullanıcınındır.** (Çıktı modu sonradan "auto-commit"e çevrilirse §6'daki not geçerli.)

---

## 4. Tur döngüsü (ajanın izleyeceği adımlar)

1. **Oku:** bu Playbook + `BACKLOG.md` + `.run-context.md` (timestamp, bu turun odak ekseni, hot-zone listesi, son turun özeti).
2. **Seç:** rotasyona (§5) ve `BACKLOG.md` önceliğine göre 1–3 ilişkili madde. Hot zone'daki madde → audit. Stable zone → apply.
3. **Doğrula (ön):** seçtiğin dosyaların gerçekten o durumda (dirty/clean) olduğunu `git status` ile teyit et.
4. **Uygula:** sınırlı, gözden geçirilebilir değişiklik. Her mantıksal adımı küçük tut.
5. **Kapıları koştur:** §7'deki 6 komut. Kırmızıysa düzelt ya da geri al — kırık bırakma.
6. **BACKLOG.md güncelle:** çözülen maddeyi işaretle, yeni doğan fırsatları ekle (kanıt + tahmini risk).
7. **Rapor yaz:** `log/<stamp>.md`'ye §6 şablonuyla. Dürüst ol: yapamadığını "yapıldı" deme.
8. **Bırak:** commit/push yapma; worktree'yi runner toplayacak.

---

## 5. Öncelik rotasyonu

Kullanıcı "sadece swarm değil, terminal de, tüm hatlar" dedi. Her tur farklı bir hatta ağırlık verir ki zamanla **tüm yüzey** iyileşsin. `.run-context.md` o turun eksenini (tur sayacı % 5) söyler; ajan o ekseni merkeze alır ama acil bir kırık tab/gate görürse onu önceler.

| Tur % 5 | Ağırlık ekseni | Tipik hedefler |
|---|---|---|
| 0 | **Tab fonksiyonel sağlık** | 9 tab render smoke, ErrorBoundary'ye düşen route, eksik test |
| 1 | **UI / UX** | design-system parity, OKLCH, ölü CSS, loading/empty/error state, a11y, toast |
| 2 | **Frontend refactor** | `Terminal.tsx` (654L), `TerminalSwarm.tsx` (511L), `OrchestratorChatPanel.tsx` (322L) bölünmesi, hook DRY |
| 3 | **Backend refactor (stable)** | `commands/swarm.rs` (2878L), `projector.rs` (2430L), `brain.rs` (2013L), `agent_dispatcher.rs` (1967L), `sidecar/terminal.rs` (2049L) modül ayrımı |
| 4 | **Borç / docs / test** | refactor-v1 ertelenenleri (Supervisor trait, MCP pool, ortak Status enum, seeds, capabilities), stale yorum, ADR, eksik birim test |

Acil kural: **kırık bir tab veya kırmızı bir kapı her zaman rotasyonu ezer.**

---

## 6. Çıktı formatı

Her tur `log/<YYYY-MM-DD_HHmm>.md` üretir. Şablon (ajan doldurur, runner kapı bloğunu ekler):

```markdown
# Otonom iyileştirme turu — <stamp>

- **Mod / eksen:** Apply|Audit / <rotasyon ekseni>
- **Snapshot:** <snap-sha> (base HEAD <head-sha>, dirty <N> dosya)
- **Seçilen maddeler:** [BACKLOG #id ...]

## Yapılanlar
- <madde>: <ne yapıldı, neden> — kanıt: <dosya:satır / test adı>

## Audit bulguları (kod değişmeden)
- <hot-zone bulgusu + öneri>

## BACKLOG değişimi
- çözülen: ... | yeni: ...

## Kapılar (runner doğruladı)
| Kapı | Sonuç |
|---|---|
| cargo check | ... |
| cargo test | ... |
| pnpm typecheck | ... |
| pnpm lint | ... |
| pnpm test (vitest) | ... |
| gen:bindings:check | ... |

## Sonuç
- ✅ READY proposal (proposals/<stamp>.patch) | 🟡 AUDIT-only | ❌ REJECTED (kapı kırık)
- maliyet: <usd/turn-süresi> · diff: +<a>/-<d>
```

Kullanıcı sabah `LATEST.md`'ye bakar → READY ise patch'i inceleyip `git apply tasks/auto-refactor/proposals/<stamp>.patch` ile dener (stable zone temizse sorunsuz uygulanır).

> **Auto-commit moduna geçilirse:** runner patch yerine ayrı bir `auto/<stamp>` dalına commit'ler ve push'lar; PR/merge kullanıcıda kalır. Bu playbook'un kuralları aynen geçerli.

---

## 7. Doğrulama kapıları (worktree içinde)

```
pnpm install --frozen-lockfile
cargo check  --manifest-path src-tauri/Cargo.toml
cargo test   --manifest-path src-tauri/Cargo.toml
pnpm --filter @neuron/app typecheck
pnpm --filter @neuron/app lint            # eslint --max-warnings=0
pnpm --filter @neuron/app exec vitest run
pnpm gen:bindings:check                   # export-bindings + git diff --exit-code bindings.ts
```

Hepsi exit 0 → READY. Biri ≠ 0 → o değişiklik REJECTED; runner çıktıyı log'a tam (kırpmasız) yazar.

**Önemli:** Bu 6 kapıyı **runner otoriter olarak** koşturur — ajan **koşturmaz** (ajan yalnız `cargo check`/`typecheck` ile hafif öz-kontrol yapar; `cargo test`/`vitest` uzun sürebildiği/kilitlenebildiği için ajan oturumunu meşgul etmez). Runner kapıları **değişen alana göre** seçer: yalnızca `app/**` değiştiyse cargo kapıları atlanır; yalnızca `src-tauri/**` değiştiyse frontend kapıları atlanır. `cargo check`/`cargo test` **timeout'ludur** (varsayılan 20dk) — asılırsa PID-ağacı öldürülür ve o kapı FAIL sayılır, tur yine de tamamlanır.

---

## 8. Maliyet & güvenlik sınırları

- **Wall-clock timeout:** tur başına varsayılan **45 dk** (`run.ps1 -TimeoutMin`). Aşılırsa süreç ağacı `taskkill /T /F` ile öldürülür, tur "TIMEOUT" raporlanır. (`claude` bu sürümde `--max-turns` sunmuyor; bütçe `--max-budget-usd` ile de sınırlanır ama abonelik auth'ta no-op olabilir — asıl sınır timeout'tur.)
- **Kapsam bütçesi:** §3.5 (1–3 madde, ≲400 satır).
- **Kilit:** üst üste binmeyi engeller; stale kilit (PID ölü) otomatik kırılır.
- **İzolasyon:** worktree + ayrı `CARGO_TARGET_DIR` → canlı `target/` ve dev build'le çakışma yok.
- **Maliyet farkındalığı:** günde 3 tur = 3 headless `claude` oturumu. Abonelik (Pro/Max) kullanımı için ek $ maliyeti yoktur ama kota tüketir. API-key auth ise `--max-budget-usd` devreye girer.

---

## 9. Backlog

Somut, önceliklendirilmiş iş listesi `BACKLOG.md`'dedir. Her tur onu okur ve günceller. İlk tohum: §5 hedefleri + 9-tab envanteri + refactor-v1 ertelenenleri.

---

## 10. Sistemi durdurma / değiştirme

- **Durdur:** `Disable-ScheduledTask -TaskName "Neuron Auto-Refactor"` (veya Görev Zamanlayıcı GUI).
- **Tek seferlik elle çalıştır:** `powershell -ExecutionPolicy Bypass -File tasks/auto-refactor/run.ps1` (veya `Start-ScheduledTask -TaskName "Neuron Auto-Refactor"`).
- **Saatleri değiştir:** `install-task.ps1` içindeki trigger'ı düzenleyip yeniden çalıştır.
- **Çıktı modunu değiştir:** `run.ps1 -Mode auto-commit` (varsayılan `proposal`).
- **Odağı sabitle:** `run.ps1 -Focus ui` gibi (rotasyonu ezer).
- Detaylar: [README.md](README.md).
