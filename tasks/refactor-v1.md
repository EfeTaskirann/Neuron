# Neuron — Refaktör Fırsatları Raporu

**İlk derleme:** 2026-04-29
**Son güncelleme:** 2026-04-29 (uygulama turu sonrası)
**Kapsam:** repo kökü, `src-tauri/`, `app/`, `docs/`, sidecar, migrasyonlar.

---

## 0. Bu turdaki uygulama özeti

Bu turda 27 maddeden **14'ü uygulandı, kod ve dokümantasyon ile çözüldü**. Geri kalan 13 madde — yüksek riskli mimari değişiklikler (Supervisor trait, ortak Status enum, cancel propagation, MCP session pool), kapsam dışı eksik özellikler (TanStack Query) ve frontend/operasyonel borçlar — açıklamaları altında **neden ertelendiği** ile birlikte not edildi.

**Uygulama disiplini:**

- Bug fix paketinin uncommit working tree'sine **dokunulmadı** — refactor değişiklikleri onun üzerine eklendi, semantiğine girilmedi.
- Her batch sonrası `cargo check --tests` + `cargo test --lib` + `pnpm typecheck` + `pnpm test` + `pnpm lint` yeşil koşturuldu.
- **Final regression:** 95 passed / 0 failed / 3 ignored (cargo) · 2 passed (vitest) · 0 lint warning · typecheck exit 0.
- `bindings.ts` regenerate edildi (`pnpm gen:bindings`). Tek satır drift: `healthDb` error tipi `string` → `AppErrorWire` (B6 fix'inin direkt sonucu, kasıtlı).

---

## 1. Çözülen maddeler (14)

| # | Madde | Etki | Kanıt |
|---|---|---|---|
| **A1** | ADR-0006 separator sapması | Dokümantasyon | `docs/adr/0006-…` §"Wire-format substitution" eklendi; ADR'ın inventory tablosu da `:` formuna çekildi |
| **A2** | `withGlobalTauri` üretim'de kapalı | Güvenlik | `src-tauri/tauri.conf.json:13` — `false`. DevTools console'dan `__TAURI_INVOKE` erişimi prod bundle'da kapatıldı |
| **A3** | `events.rs` sabit modülü | Tip güvenliği | Yeni dosya `src-tauri/src/events.rs`. 6 emit-site (agents/mailbox/mcp×2/runs:span/panes:line) sabit/helper'a bağlandı; 3 testle "no `.` in any event name" invariant'ı doğrulanıyor |
| **A4** | ID format ADR-0007 | Sözleşme | Yeni `docs/adr/0007-id-strategy.md`: prefixed-ULID + slug + autoincrement-int üçü için kullanım kuralı. Charter §"Hard constraints" #9 olarak da çapalandı |
| **A5** | Timestamp invariant | Sözleşme | Charter §"Hard constraints" #8: `_at`=saniye, `_ms`=milisaniye, başka format yok. `src-tauri/src/time.rs` modülü `now_seconds`/`now_millis` pair'ini tek yerden veriyor |
| **B1** | Test mock helper konsolide | DRY | `test_support::mock_app_with_pool` + `mock_app_with_pool_and_terminal_registry` eklendi. 6 dosyadaki lokal kopya silindi (~120 satır net düşüş) |
| **B2** | SQL projection sabiti | DRY | `commands/agents.rs:48` `const AGENTS_COLS = "id, name, model, temp, role"` — agents_list/get/update üçü aynı kaynaktan |
| **B3** | `now_seconds` tek yer | DRY | Yeni `src-tauri/src/time.rs`. `commands/runs.rs` ve `commands/mailbox.rs`'in lokal kopyaları silindi |
| **B5** | `lib.rs` collect_commands grouping | Okunabilirlik | Komutlar namespace bloklarına ayrıldı (kosmetik); tauri-specta'nın `collect_commands!` tek-listesi korundu |
| **B6** | `health_db` AppError'a normalize | Sözleşme | `Result<DbHealth, String>` → `Result<DbHealth, AppError>`. Frontend artık tek error wire shape kullanıyor; bindings.ts `healthDb` artık `AppErrorWire` döner |
| **C3** | Sidecar IPC framing ADR | Dokümantasyon | Yeni `docs/adr/0008-sidecar-ipc-framing.md`: length-prefixed (in-house) vs NDJSON (MCP spec) seçim kuralı. Üçüncü sidecar'a precedent |
| **D4** | Test seed dağınıklığı | DRY | B1 ile birlikte konsolide edildi: tüm test modülleri `test_support::{mock_app_with_pool, fresh_pool, seed_*}` üzerinden import ediyor |
| **E3** | `.bridgespace/` gitignore | Operasyonel | (Bug fix paketi turunda kapanmıştı; takip için listede tutuldu) |
| **F1** | bindings regen + drift guard | Tooling | `package.json` scripts: `gen:bindings`, `gen:bindings:check` (`git diff --exit-code` ile CI guard); bindings regenerate edildi |
| **F4** | `pnpm dev` ergonomi | Operasyonel | `pnpm dev` → `tauri dev` (full app), `pnpm dev:web` saf Vite preview. Aynı şekilde `pnpm build` |

---

## 2. Çözüm parçası uygulanan maddeler (3)

### G1 — `runs:cancel` race kapatıldı (sidecar sinyali açık) 🟡
- **Bug paketinin sağladığı:** atomic `UPDATE … WHERE status='running'` (`commands/runs.rs`) + `finalise_run` aynı guard altında (`sidecar/agent.rs:483`).
- **Hâlâ açık:** Sidecar'a iptal sinyali (Python tarafına `cancel_run` frame'i) yok.
- **Neden bu turda yapılmadı:** Python `__main__.py`'a yeni mesaj tipi + asyncio cancel propagation — kod kapsamı sidecar protokolünü değiştirir; bu turun "kodu bozma" şartı altında riskli.

### C2 — `runs.status` artık `cancelled` (ortak enum yok) 🟡
- **Bug paketinin sağladığı:** migration `0002_constraints.sql:80` CHECK genişletildi.
- **Hâlâ açık:** Terminal `status` ile run `status` ayrı enum'lar; ortak `domain::Status` yok.
- **Neden bu turda yapılmadı:** Ortak enum DB CHECK constraint'lerini birleştirmek demek; mevcut testler 5 vs 4 state'i ayrı sayıyor — refactor riskli, frontend WP-W2-08 sözleşmesini bekliyor.

### C4 — MCP `protocolVersion` validation (Python↔Rust handshake yok) 🟡
- **Bug paketinin sağladığı:** `mcp/client.rs:215` initialize cevabında protocolVersion zorunlu.
- **Hâlâ açık:** Rust↔Python framing'in version handshake'i yok.
- **Neden bu turda yapılmadı:** Python tarafı (`agent_runtime/__main__.py`, `framing.py`) eş zamanlı uyarlama gerekir; iki dil değişikliğini tek refactor'da yapmak "kodu bozmadan" şartını gerer.

---

## 3. Bu turda ertelenen maddeler (10)

Hepsinde sebep aynı: **kodu bozma riski** veya **kapsam dışı**. Her madde için neden ve nasıl yapılması gerektiği aşağıda.

### B4 — `agents_update` dinamik SQL builder
- **Risk:** `sqlx::QueryBuilder` API'sine geçiş; mevcut testler manuel `bind()` sırasını doğruluyor — builder'a geçerken bind sırası bozulursa runtime hata.
- **Yapılması:** ayrı bir PR'da, builder migration ile birlikte yeni unit testler (rastgele sıralı patch'lerle).

### C1 — `Supervisor` trait soyutlaması
- **Risk:** Hem `agent.rs`'in `SidecarHandle` hem `terminal.rs`'in `TerminalRegistry` impl'ini değiştirir. Test coverage var ama trait üzerinden yeniden yazım large-diff.
- **Yapılması:** üçüncü bir sidecar (vector store, observability) eklenirken aynı PR'da trait'i çıkar; iki impl'i incremental dönüştür.

### C5 — MCP session pool + pending request map
- **Risk:** Mimari değişim. Şu an her tools/list ve callTool yeni spawn — pool'a geçerken cancel-safety, request id correlation, pool eviction yeniden tasarım.
- **Yapılması:** Week 3 başlangıcında ayrı WP. Bu turun TODO yorumu yeterli.

### D1 — Migration domain bölünmesi
- **Risk:** SQLite migration'ları sıralı; mevcut `0001_init.sql` + `0002_constraints.sql` üzerinden split refactor değil "şema reset" anlamına gelir. Greenfield olduğu için çıkar yok.
- **Yapılması:** Week 3 production öncesi tek squash kararı. İleri migrasyonlar zaten `0003_…`, `0004_…` ile domain bazlı eklenebilir.

### D2 — Seeds modülü konsolidasyonu
- **Risk:** Bug fix paketi `seed_mcp_servers`'ı yeni `parse_report` API'sine taşıdı (K6 fix). Aynı dosyaya elle dokunmak, paketin commit edilmemiş diff'iyle conflict riski yaratır.
- **Yapılması:** Bug fix paketi commit edildikten sonra ayrı bir PR'da `src-tauri/src/seeds/` modülü.

### D3 — Demo workflow seed kalıcılık
- **Risk:** Düşük (sadece doc güncellemesi) — atlandı çünkü WP-W2-08 spec'i bekliyor; o WP'de fixture sistemi geldiğinde seed taşıma planı orada yazılır.

### E1 — Capabilities daraltma
- **Risk:** Tauri 2 capability sistemi command-bazlı; her komut için manifest girdisi gerekir. Frontend WP-W2-08 başlamadan komut surface'ı son halini almadığı için erken sıkma sonra başka komutu kırabilir.
- **Yapılması:** WP-W2-08 sonunda, tüm komut listesi sabitlendiğinde.

### E2 — Pre-commit hook (Co-Author trailer)
- **Risk:** `.git/hooks/` repo-level değil, kullanıcı-level bir setup. Husky veya `core.hooksPath` adopt etmeden eklenirse kullanıcı sistemini etkileyemez.
- **Yapılması:** Husky veya benzer bir tooling ADR'ı + repo-level hooks/ dizini ile aynı PR.

### F2 — TanStack Query bağımlılığı
- **Yapılması:** WP-W2-08 başlangıç adımı. Bu refactor değil, eksik özellik.

### F3 — Windows manifest ADR
- **Risk:** Düşük; `build.rs` zaten epey iyi yorumlu (line 11-27). Yeni ADR yazmak değil bilgi tekrarı; existing yorumun ADR-formuna gelmesi düşük getirili.
- **Yapılması:** İlerideki Windows-spesifik bir bug çıktığında o sırada ADR yapılır.

### F5 — bindings.ts `any` cast
- **Risk:** tauri-specta crate'inin upgrade'ini bekler; refactor değil patch'tir.

### G2 — MCP install UX (Filesystem dışı 5 stub server)
- **Risk:** Frontend tarafı (Marketplace UI) dahil; manifest'lere `installable: bool` field'i eklemek bindings drift yaratır + WP-W2-08 frontend yazmadan görsel etkisi anlaşılmaz.
- **Yapılması:** WP-W2-08 Marketplace route ile aynı PR.

---

## 4. Yeni doğan / büyüyen refaktör fırsatları (önceki turdan kalan)

Bu turda not edildi ama kod yazılmadı — Tier 2/3 olarak duruyorlar.

- **Magic timing/ölçek sabitlerinin dağınıklığı** (`SHUTDOWN_GRACE`, ring 5000/1000, `MAX_PENDING_BYTES`, `READ_CHUNK_BYTES`). `tuning.rs` modülü adayı.
- **`eprintln!` audit + `tracing` adopt et.** report.md K6, K1 fix'leri ve mcp/client.rs version drift bilgisi structured log'a hak ediyor.
- **Compensating-action helper.** `commands/runs.rs:139-149` rollback inline; başka komutlar (mcp:install commit→emit gibi) aynı şablonu tekrarlayacak.
- **`mcp/client.rs` pending request map.** Şu an "client never overlaps requests" yorumda; session pool refactor'ında (C5) test'le doğrulanmalı.

---

## 5. Yapılan değişikliklerin dosya envanteri

### Yeni dosyalar
- `docs/adr/0007-id-strategy.md`
- `docs/adr/0008-sidecar-ipc-framing.md`
- `src-tauri/src/events.rs` (3 birim test dahil)
- `src-tauri/src/time.rs` (2 birim test dahil)

### Güncellenen dosyalar
- `docs/adr/0006-event-naming-and-mailbox-realtime.md` (Wire-format substitution bölümü + tablo `:` formuna)
- `PROJECT_CHARTER.md` (Hard constraints #8 timestamp + #9 ID strategy)
- `src-tauri/src/lib.rs` (mod events, mod time, commands grouping)
- `src-tauri/src/test_support.rs` (mock_app_with_pool×2 ortak helper)
- `src-tauri/src/commands/{agents,health,mailbox,mcp,runs,terminal,workflows}.rs` (events/time/test_support entegrasyonu, AGENTS_COLS, AppError normalize)
- `src-tauri/src/sidecar/{agent,terminal}.rs` (events::run_span / events::pane_line)
- `src-tauri/tauri.conf.json` (withGlobalTauri: false)
- `package.json` (gen:bindings, dev ergonomi)
- `app/src/lib/bindings.ts` (regenerate, +1 / -1 — healthDb error tipi)

### Dokunulmayan dosyalar (bilinçli)
- Bug fix paketinin uncommit değişiklikleri (`commands/runs.rs` cancel kod yolu, `sidecar/{agent,terminal}.rs` shutdown disiplini, `mcp/{client,manifests}.rs`, `migrations/0002_constraints.sql`).

---

## 6. CI gateleri (final durum)

| Gate | Sonuç |
|---|---|
| `cargo check --tests` | exit 0 |
| `cargo test --lib` | **95 passed**, 0 failed, 3 ignored (5 yeni: 3 events + 2 time) |
| `pnpm typecheck` | exit 0 |
| `pnpm test --run` (vitest) | 1 file, 2 tests passed |
| `pnpm lint` | exit 0 (no warnings) |
| `pnpm gen:bindings` | success — 1 satır drift, kasıtlı |

---

## 7. Bu raporun bundan sonraki kullanımı

Sıradaki refactor turunda öncelik:
1. **G1 kalanı** (sidecar cancel sinyali) — kullanıcı UX'ini doğrudan etkiliyor
2. **C1** (Supervisor trait) — üçüncü sidecar gelmeden cement et
3. **C2** (ortak Status enum) — frontend WP-W2-08 sözleşmesi netleşince
4. **D2** (seeds modülü) — bug fix paketi commit edildikten sonra
5. **E1** (capabilities) — WP-W2-08 sonu

Bu rapor uncommit değişiklikleri **fiilen uyguladığı için** yeni `git status` çıktısında 20+ dosya görünecektir. İlk öneri: bu turun değişikliklerini **bug fix paketinden ayrı bir commit** olarak almak (`refactor:` prefix), çünkü iki paket bağımsız olarak gözden geçirilebilir.
