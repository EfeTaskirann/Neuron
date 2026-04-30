# Sub-Agent Briefs — 2026-04-29

**Orchestrator:** parent session (Efe Taşkıran)
**Hedef:** WP-W2-08 öncesi sözleşme tamamlama (Ajan A/B/C) + operasyonel hijyen (Ajan D).
**Dispatch modeli:** 4 farklı terminal, 4 paralel ana ajan. Her brief self-contained ama §0 ortak çerçeve her ajana paste edilir.
**Önerilen sıra:** **B → C → A → D**. D en son, çünkü A/B/C'nin eklediği yeni `eprintln!` çağrılarını da `tracing` çevirisine dahil etmesi gerek.
**Ortak referans:** `tasks/refactor.md` §3 (ertelenmiş madde gerekçeleri) + §4 (yeni doğan refaktör fırsatları). Bu dosyadaki ③–⑩ numaralandırması orchestrator'un 2026-04-29 tarihli "eksiklik sıralaması" listesinden gelir.

---

## Kullanım

Her ajan terminaline **iki bölüm birlikte** paste edilir:

1. **§0 Ortak çerçeve** (her ajan için aynı — otorite, yasaklar, çıktı formatı, çakışma haritası)
2. **Kendi ajan bölümü** (§1 / §2 / §3 / §4'ten biri)

Ajan brief'ini okuduktan sonra listelenen otorite dosyalarını kendisi açar, allowlist dışına çıkmaz, çıktı formatını izler. Orchestrator §5'teki merge protokolüyle 4 worktree'yi ana branch'e entegre eder + tek seferde `bindings.ts` regen yapar.

---

## §0. Ortak çerçeve (her ajana paste edilir)

### 0.1 Otorite hierarchy (önce oku)

Sırasıyla:

1. `PROJECT_CHARTER.md` — özellikle:
   - Constraint #1: "Frontend mock shape is the contract" + 2026-04-29 *display-derived carve-out* paragrafı
   - Constraint #8: timestamp invariant (`_at` epoch saniye, `_ms` epoch milisaniye)
   - Constraint #9: identifier strategy (ADR-0007)
   - Tech stack tablosu (yeni dependency = Charter ihlâli; yalnızca Ajan D'ye yetki)
2. `AGENTS.md` — sub-agent çalışma kuralları, "hard rules" bölümü, commit disiplini
3. `tasks/refactor.md` §3 ve §4 — backlog ve gerekçeler
4. ADR'lar — her brief kendi listesini verir
5. Brief'in **kendi ajan bölümü** (§1 / §2 / §3 / §4)

### 0.2 Yasaklar (her ajana uygulanır)

| Yasak | Gerekçe |
|---|---|
| `bindings.ts` regen | Orchestrator tek seferde regen yapar — 4 ajanın paralel regen'i drift çoğullar |
| `pnpm` / `npm` komutu | Sen Rust-only ajansın. Frontend doğrulama orchestrator yapar (Ajan C `lib.rs`'e dokunduğu hariç, ama yine `pnpm` çalıştırmaz) |
| `Neuron Design/` veya `neuron-docs/` | Reference-only dizinler. **İstisna:** yalnızca Ajan B `data.js`'e dokunabilir (gerekçe brief'inde yazılı) |
| Allowlist dışında write | Read serbest, write değil. Allowlist brief'in "Allowlist" bölümünde verilir |
| `--no-verify` veya hook bypass | Hook fail olursa kök nedeni çöz |
| Yeni Cargo.toml dependency | Yalnızca Ajan D'ye yetki (tracing crate'leri) |
| Yeni Tauri command / endpoint | Yalnızca Ajan C'ye yetki (`me:get`) |
| Davranış değişikliği | Yalnızca Ajan A scope'u (yeni alanlar + reader path) ve Ajan C (yeni komut). B ve D davranış-nötr |
| `bindings.ts`'i manuel düzenleme | Generated dosya; eslint ignore listesinde |

### 0.3 Çıktı formatı (brief sonunda kullanılır)

```
✅/❌ Ajan {A|B|C|D} — {başlık}
- files changed: N (yeni: x, modified: y)
- acceptance: per-item pass/fail (her acceptance criterion için)
- cargo check --tests: exit 0 / exit N
- cargo test --lib {scope}: X passed / Y failed / Z ignored
- diff stat: <git diff --stat çıktısı>
- known caveats: <varsa kısa not, yoksa "yok">
- handoff to orchestrator: <orchestrator'un yapması gereken ek iş — örn. bindings regen, refactor.md ✅ işaretle>
```

### 0.4 Çakışma haritası

| Dosya | Ajan A | Ajan B | Ajan C | Ajan D |
|---|---|---|---|---|
| `models.rs` | `Pane` struct + `ApprovalBanner` | – | `Me`/`User`/`Workspace` (dosya sonu) | – |
| `sidecar/terminal.rs` | reader regex genişletmesi | – | – | constants → tuning + eprintln → tracing |
| `sidecar/agent.rs` | – | – | – | constants → tuning + eprintln → tracing |
| `mcp/client.rs` | – | – | – | constants → tuning + eprintln → tracing |
| `db.rs` | – | seed test ID listesi | – | eprintln → tracing |
| `lib.rs` | – | – | `collect_commands!` +1 satır | `mod tuning; mod util;` + subscriber init |
| `commands/runs.rs` | – | – | – | rollback inline → util helper |
| `commands/terminal.rs` | `terminal_list` SELECT genişle | – | – | – |
| `commands/me.rs` | – | – | yeni dosya | – |
| `commands/util.rs` | – | – | – | yeni dosya |
| `commands/mod.rs` | – | – | `pub mod me;` | `pub mod util;` |
| `tuning.rs` | – | – | – | yeni dosya |
| `mcp/manifests*` | – | 6 yeni JSON + `manifests.rs` liste | – | – |
| `migrations/0003_*` | yeni `panes_approval` | – | – | – |
| `Neuron Design/app/data.js` | – | s1–s12 → slug realign | – | – |
| `Cargo.toml` (src-tauri) | – | – | – | tracing deps |

**Paylaşılan dosyalar (sequential merge gerek):**
- `models.rs`: A (Pane) ↔ C (Me/User/Workspace) — farklı struct, dosya sonu ekleme; text çakışma yok ama context'te tutarsızlık olmasın
- `sidecar/terminal.rs`: A (reader) ↔ D (constants + tracing) — D, A'nın diff'inden sonra dispatch edilir
- `db.rs`: B (test) ↔ D (tracing) — D sonra
- `lib.rs`: C (+1 satır) ↔ D (subscriber init) — D sonra

**Dispatch sırası:** **B → C → A → D**. Worktree isolation kullanılırsa hepsi paralel başlatılabilir; D'nin worktree'si A/B/C merge sonrası fast-forward edilmeli.

### 0.5 Genel acceptance (her ajan için ek)

- `cargo check --manifest-path src-tauri/Cargo.toml --tests` → exit 0
- `cargo test --manifest-path src-tauri/Cargo.toml --lib` → 0 failure (yeni testler eklenirse pass count yükselir)
- `git diff --stat` çıktısı yalnızca brief'in allowlist'indeki dosyaları göstermeli
- Hiçbir uncommitted değişikliğe dokunulmamalı (working tree'de kullanıcı çalışması varsa stash et veya kendi worktree'sinde çalış)
- `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer'ı (commit yapılırsa)

### 0.6 Acil durum protokolü

Brief'te **belirsizlik** veya **otorite çelişkisi** çıkarsa:
- Sub-agent'sın — orchestrator'a (parent session) handoff yap, varsayım yapma.
- Allowlist dışına çıkma zorundaysan: durmak ve handoff yap.
- Test fail olursa: kök nedeni bul, fix; geçmek için test devre dışı bırakma.

---

## §1. Ajan A — Pane domain (kalemler ③ + ④)

### 1.1 Sen kimsin

WP-W2-06 follow-up'ı yapan tek-amaç ajansın. Mock `terminal-data.js`'in `Pane` shape'iyle backend `models::Pane` arasındaki açığı kapatacaksın:
- `tokensIn` / `tokensOut` / `costUsd` (Week 2 boyunca `null` döner — Week 3 spans-derived)
- `uptime` (`null` döner — frontend hook hesaplar, Charter #1 *display-derived carve-out*)
- `approval` (`{tool, target, added, removed}` banner — `awaiting_approval` durumundaki pane'ler için backend desteği)

### 1.2 Otorite (oku)

- `PROJECT_CHARTER.md` Constraint #1 *carve-out* paragrafı, Constraint #8
- `NEURON_TERMINAL_REPORT.md` — Pane shape, state machine, agent-spesifik regex tablosu
- `Neuron Design/app/terminal-data.js` — `p1`'in `approval: {tool: "write_file", target: "src/components/Button.tsx", added: 47, removed: 12}` şekli
- `docs/adr/0007-id-strategy.md` (mevcut `panes.id` zaten `p-{ULID}`, dokunmuyorsun)
- `docs/adr/0006-event-naming-and-mailbox-realtime.md` (`panes:{id}:line` zaten doğru, dokunmuyorsun)
- `tasks/refactor.md` §3.3'teki ③ ve ④ kalemleri

### 1.3 Allowlist (yalnızca bu dosyalara write)

- `src-tauri/src/models.rs` — `Pane` struct'a 5 alan + yeni `ApprovalBanner` struct
- `src-tauri/migrations/0003_panes_approval.sql` (yeni)
- `src-tauri/src/sidecar/terminal.rs` — reader-side approval blob extraction
- `src-tauri/src/commands/terminal.rs` — `terminal_list` SELECT genişletmesi

### 1.4 Görevler

**Görev 1 — `models.rs` Pane struct genişletme:**

Mevcut `Pane` struct'a 5 yeni alan (sırayla, mevcut alanlardan sonra):

```rust
/// Aggregate input tokens consumed by this pane's agent runtime.
/// Week 2: always None — populated in Week 3 from runs_spans.
pub tokens_in: Option<i64>,
/// Aggregate output tokens.
pub tokens_out: Option<i64>,
/// Aggregate USD cost.
pub cost_usd: Option<f64>,
/// Display-derived "12m 04s" string. Per Charter Constraint #1
/// carve-out: backend ships None; frontend hook computes from
/// started_at. Field exists for mock-shape parity only.
pub uptime: Option<String>,
/// Approval banner blob extracted from the most recent
/// `awaiting_approval` regex match. None when the pane is not
/// awaiting approval AND has never been.
pub approval: Option<ApprovalBanner>,
```

Yeni struct (Pane'in altına ekle, terminal block'unda):

```rust
/// One approval banner blob. Populated by the terminal reader when
/// an `awaiting_approval` regex matches; surfaced to the UI's amber
/// banner strip per `NEURON_TERMINAL_REPORT.md`. Mock parity:
/// `terminal-data.js#panes[0].approval`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalBanner {
    pub tool: String,
    pub target: String,
    pub added: i64,
    pub removed: i64,
}
```

**Görev 2 — Migration `0003_panes_approval.sql`:**

```sql
-- 0003 — Pane approval banner persistence (③+④).
-- The terminal reader extracts {tool, target, added, removed} from
-- regex matches and serialises to JSON in this column. Read by
-- commands::terminal::terminal_list and surfaced as Pane.approval.
ALTER TABLE panes ADD COLUMN last_approval_json TEXT;
```

SQLite ALTER TABLE ADD COLUMN idempotent değildir (re-run hata verir), ama sqlx migrator `_sqlx_migrations`'a kayıt aldığı için **migrator-level idempotent**. Test güncellemesi gerek (Görev 5).

**Görev 3 — `sidecar/terminal.rs` reader extension:**

Mevcut `awaiting_approval` regex match'inden sonra approval blob'unu çıkarmaya çalış:

- Match success → en basit Week 2 davranışı: `ApprovalBanner { tool: "unknown".into(), target: String::new(), added: 0, removed: 0 }` placeholder kabul edilebilir; agent_kind'e göre daha akıllı parse opsiyonel (nice-to-have, scope dışı değil).
  - claude-code daha agresif parse: regex `r"(?m)^Tool:\s*(?P<tool>\S+).*\n.*target:\s*(?P<target>\S+).*\n.*\+(?P<add>\d+).*-(?P<rem>\d+)"` — eşleşmezse placeholder kullan.
  - codex / gemini için Week 2'de placeholder yeterli.
- `serde_json::to_string(&banner)?` ile string'e çevir, `panes.last_approval_json`'a `UPDATE panes SET last_approval_json = ? WHERE id = ?` yaz.
- `awaiting_approval` durumundan çıkıldığında kolon **temizlenmiyor** — son seen banner kalıcı kalır. `terminal_list` döndürmeden filtre uygular: `pane.status == "awaiting_approval"` ise `Some(banner)`, değilse `None`. Doc-comment ile bu kararı açıkla.

**Görev 4 — `commands/terminal.rs::terminal_list`:**

SELECT'i genişlet:
- `last_approval_json TEXT` kolonu çekilsin
- `tokens_in / tokens_out / cost_usd / uptime` 4 alan: `SELECT NULL AS tokens_in, NULL AS tokens_out, NULL AS cost_usd, NULL AS uptime, ...` — query-level None sağlamak için
- Pane row'a deserialize ederken: `approval = if status == "awaiting_approval" { last_approval_json.and_then(|s| serde_json::from_str(&s).ok()) } else { None }`

**Görev 5 — Test güncellemeleri:**

`db::tests::migrations_are_idempotent` testinde `assert_eq!(count, 2, ...)` → `assert_eq!(count, 3, "three migrations recorded (0001+0002+0003)")` haline getir.

Yeni testler ekle (`commands::terminal::tests` veya `sidecar::terminal::tests`):
- `pane_with_awaiting_approval_and_blob_returns_banner` — pane row insert (status='awaiting_approval', last_approval_json=`{"tool":"x","target":"y","added":1,"removed":2}`), `terminal_list` çağır, `approval` parsed kontrol
- `pane_with_idle_status_returns_none_approval` — idle pane'in `approval` field'ı None
- `pane_with_null_blob_returns_none` — kolon NULL'sa None

### 1.5 Yapma

- `terminal_spawn` davranışını değiştirme — sadece `terminal_list` SELECT'ini değiştir
- `bindings.ts` regen
- LangGraph sidecar / MCP / mailbox dosyalarına dokunma
- Yeni endpoint ekleme (banner için ayrı `terminal:approval` komutu yapma — `terminal_list` yeterli)
- `awaiting_approval` regex'inin temel davranışını değiştirme — sadece blob extraction ekleme

### 1.6 Acceptance criteria (self-verify)

- [ ] `Pane` 5 yeni alan, hepsi camelCase wire serde rename'le doğru
- [ ] `ApprovalBanner` mock parity (4 alan, camelCase)
- [ ] Migration 0003 dosyası mevcut, sqlx migrator yeni-DB ve eski-DB üzerinde de çalışıyor
- [ ] `awaiting_approval` durumunda `last_approval_json` populated; idle'a düştüğünde DB satırı kalır ama `terminal_list` `None` döndürür
- [ ] `terminal_list` döndürdüğü Pane'lerde tokensIn/tokensOut/costUsd/uptime hep `null`
- [ ] Yeni 3+ test geçiyor
- [ ] `db::tests::migrations_are_idempotent` 2 → 3 update edilmiş
- [ ] cargo test --lib full pass (regression yok; önceki 95+ yeni testler)
- [ ] cargo check temiz

### 1.7 Doğrulama komutları

```bash
cargo check --manifest-path src-tauri/Cargo.toml --tests
cargo test --manifest-path src-tauri/Cargo.toml --lib sidecar::terminal
cargo test --manifest-path src-tauri/Cargo.toml --lib commands::terminal
cargo test --manifest-path src-tauri/Cargo.toml --lib db::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib   # full regression
```

### 1.8 Çakışma uyarısı (orchestrator için bilgi)

- `sidecar/terminal.rs` Ajan D ile paylaşılan dosya. Sen reader path'ine ekleme yaparsın; D constants ve eprintln satırlarına dokunur. Worktree isolation altında orchestrator merge'inde text-level kolay; semantic çakışma yok çünkü D'nin scope'u davranış-nötr.
- `models.rs` Ajan C ile paylaşılan. Sen `Pane`'e ek satır + yeni `ApprovalBanner`; C dosyanın **sonuna** Me/User/Workspace ekler. Diff'leriniz çakışmaz.

### 1.9 Çıktı

§0.3 formatını kullan.

---

## §2. Ajan B — MCP catalog tamamla (kalemler ⑥ + ⑦)

### 2.1 Sen kimsin

WP-W2-05 follow-up'ı yapan tek-amaç ajansın. Mevcut backend yalnızca 6 MCP sunucusu seediyor (filesystem, github, postgres, browser, slack, vector-db) ama mock `data.js#servers` 12 sunucu listeler (`s1–s12`). Bu açığı **iki yönden** kapatacaksın:

- Mock `data.js`'i textual ID'lere geçir (`s1` → `filesystem`, …, `s12` → `memory`)
- Eksik 6 sunucuyu manifest stub olarak ekle (Linear, Notion, Stripe, Sentry, Figma, Memory)

### 2.2 Otorite (oku)

- `PROJECT_CHARTER.md` Constraint #1
- `docs/adr/0007-id-strategy.md` §"Author-stable slugs" — mevcut tasarımı izliyorsun
- `docs/work-packages/WP-W2-05-mcp-registry.md` (varsa, manifest deseni için)
- `src-tauri/src/mcp/manifests/browser.json` ve `postgres.json` — mevcut **catalog-only stub** deseni (`spawn: null`, `requires_secret: null`)
- `src-tauri/src/mcp/manifests.rs` — mevcut `ALL_MANIFESTS_JSON` listesi
- `src-tauri/src/db.rs::seed_mcp_servers_is_idempotent` — güncellenecek test
- `tasks/refactor.md` §3.3'teki ⑥ ve ⑦ kalemleri

### 2.3 Allowlist

- `src-tauri/src/mcp/manifests/linear.json` (yeni)
- `src-tauri/src/mcp/manifests/notion.json` (yeni)
- `src-tauri/src/mcp/manifests/stripe.json` (yeni)
- `src-tauri/src/mcp/manifests/sentry.json` (yeni)
- `src-tauri/src/mcp/manifests/figma.json` (yeni)
- `src-tauri/src/mcp/manifests/memory.json` (yeni)
- `src-tauri/src/mcp/manifests.rs` (yalnızca `ALL_MANIFESTS_JSON`'a 6 satır ekleme)
- `src-tauri/src/db.rs` (yalnızca `seed_mcp_servers_is_idempotent` testindeki `expected` ID listesi)
- `Neuron Design/app/data.js` (mock realign — **istisna gerekçesi:** dosya WP-W2-08'de silinecek, WP-08 başlamadan mock-shape parity için orchestrator-onaylı bir-kerelik düzenleme. AGENTS.md "do NOT edit Neuron Design/" kuralı normal çalışma akışı için; bu istisna brief'te yetkilidir)

### 2.4 Görevler

**Görev 1 — 6 yeni stub manifest:**

Mock `data.js#servers[]` array'ından metadata kopyala. Her dosya şu şekilde:

```json
{
  "id": "linear",
  "name": "Linear",
  "by": "Linear",
  "description": "Create issues, search projects, update statuses across your Linear workspace.",
  "installs": 2900,
  "rating": 4.6,
  "featured": false,
  "spawn": null,
  "requires_secret": null,
  "default_root_kind": null
}
```

6 dosya (mock'tan birebir kopyala — `id` slug, geri kalanı mock değerleri):

| Slug | name | by | description (kopyala) | installs | rating | featured |
|---|---|---|---|---|---|---|
| `linear` | Linear | Linear | "Create issues, search projects, update statuses across your Linear workspace." | 2900 | 4.6 | false |
| `notion` | Notion | Notion | "Read and write pages, databases, and blocks with token-scoped access." | 5600 | 4.7 | false |
| `stripe` | Stripe | Stripe | "Inspect customers, charges, and subscriptions. Live and test mode supported." | 1800 | 4.8 | false |
| `sentry` | Sentry | Sentry | "Pull recent errors, breadcrumbs, and release health into the agent context." | 1400 | 4.5 | false |
| `figma` | Figma | Figma | "Read frames and components from a file URL; export PNG/SVG to the workspace." | 3700 | 4.4 | false |
| `memory` | Memory | Anthropic | "Long-term key-value store scoped to the workspace; safe for cross-run continuity." | 2100 | 4.6 | false |

Hepsi `spawn: null` → install denenirse `McpServerSpawnFailed` (mevcut davranış, refactor.md G2'de Week 3'e ertelendi).

**Görev 2 — `manifests.rs` listesine ekle:**

```rust
pub const ALL_MANIFESTS_JSON: &[(&str, &str)] = &[
    ("filesystem", include_str!("manifests/filesystem.json")),
    ("github", include_str!("manifests/github.json")),
    ("postgres", include_str!("manifests/postgres.json")),
    ("browser", include_str!("manifests/browser.json")),
    ("slack", include_str!("manifests/slack.json")),
    ("vector-db", include_str!("manifests/vector-db.json")),
    // 6 new catalog-only stubs (2026-04-29) — mock parity per Charter #1
    ("linear", include_str!("manifests/linear.json")),
    ("notion", include_str!("manifests/notion.json")),
    ("stripe", include_str!("manifests/stripe.json")),
    ("sentry", include_str!("manifests/sentry.json")),
    ("figma", include_str!("manifests/figma.json")),
    ("memory", include_str!("manifests/memory.json")),
];
```

**Görev 3 — `Neuron Design/app/data.js` realign:**

`servers: [...]` array'ında her satırın `id: "sN"` → textual slug:

| Eski | Yeni |
|---|---|
| `s1` Filesystem | `filesystem` |
| `s2` GitHub | `github` |
| `s3` PostgreSQL | `postgres` |
| `s4` Browser | `browser` |
| `s5` Slack | `slack` |
| `s6` Vector DB | `vector-db` |
| `s7` Linear | `linear` |
| `s8` Notion | `notion` |
| `s9` Stripe | `stripe` |
| `s10` Sentry | `sentry` |
| `s11` Figma | `figma` |
| `s12` Memory | `memory` |

Sonra `Neuron Design/app/`'i grep et: `s1`, `s2`, ..., `s12` referansları başka dosyada (`shell.jsx`, `routes.jsx`, `inspector.jsx`) varsa onları da güncelle. Mock UI'sının mevcut hali kırılmasın.

**Mock'taki `installed: true|false` flag'lerine DOKUNMA:**
- s1, s2, s5 (Filesystem/GitHub/Slack) `installed: true` mock'ta → backend seed'i `installed: 0` veriyor → mismatch korunuyor
- Bu mismatch refactor.md G2 ile Week 3'te `default_installed` flag'iyle çözülecek; **scope dışı**, burada elleme. Known caveat olarak handoff'a yaz.

**Görev 4 — `db.rs` test güncellemesi:**

`seed_mcp_servers_is_idempotent` testinde:

```rust
let mut expected = vec![
    "browser", "figma", "filesystem", "github", "linear",
    "memory", "notion", "postgres", "sentry", "slack",
    "stripe", "vector-db",
];
// expected.sort();  // alfabetik için varsa zaten sıralı
```

Ve `assert_eq!(after_first, 6, ...)` → `assert_eq!(after_first, 12, "all twelve manifests seeded")`.

### 2.5 Yapma

- `seed_mcp_servers` fonksiyonunun davranışını değiştirme — sadece test'in expected'ı güncel
- Yeni manifest'lere `spawn` field'ı yazma (catalog-only stub kalsın)
- `installed` default'unu değiştirme (Week 3 işi)
- `mcp/manifests.rs::ServerManifest` struct'ına yeni alan ekleme
- `bindings.ts` regen
- Frontend kodu (`shell.jsx` dahil) işlevsel davranışını değiştirme — sadece ID literal'lerini değiştir

### 2.6 Acceptance criteria

- [ ] 6 yeni manifest dosyası, mock metadata ile birebir
- [ ] `manifests.rs::ALL_MANIFESTS_JSON` 12 girdi
- [ ] `data.js#servers` 12 satır, hepsi slug ID
- [ ] `Neuron Design/app/`'te `s1`..`s12` referansı kalmamış (grep ile doğrula)
- [ ] `seed_mcp_servers_is_idempotent` testinde 12 ID, sıralı
- [ ] cargo test --lib seed_mcp_servers yeşil
- [ ] cargo test --lib mcp::manifests yeşil
- [ ] cargo test --lib full pass (regression yok)
- [ ] cargo check temiz

### 2.7 Doğrulama komutları

```bash
cargo check --manifest-path src-tauri/Cargo.toml --tests
cargo test --manifest-path src-tauri/Cargo.toml --lib mcp::manifests
cargo test --manifest-path src-tauri/Cargo.toml --lib db::tests::seed_mcp_servers_is_idempotent
cargo test --manifest-path src-tauri/Cargo.toml --lib   # full regression
```

Mock realign için manuel doğrulama:

```bash
# Repo root'tan:
grep -rn '"s[0-9]' "Neuron Design/app/" || echo "No s1-s12 literals remain"
```

### 2.8 Çakışma uyarısı

- `db.rs` Ajan D ile paylaşılan. Sen test'in `expected` array'ına dokunuyorsun; D `seed_mcp_servers` içindeki `eprintln!`'leri `tracing::warn!`'a çeviriyor. Farklı satırlar.

### 2.9 Çıktı

§0.3 formatını kullan. **Ek handoff:** `default_installed` mismatch'i refactor.md G2 olarak Week 3'e bilinçli erteleniyor — orchestrator'a "G2 hâlâ açık" hatırlatması.

---

## §3. Ajan C — me:get komutu (kalem ⑤)

### 3.1 Sen kimsin

Mock `data.user` (`{initials, name}`) ve `data.workspace` (`{name, count}`) için tek read-only `me:get` Tauri komutu yazacaksın. Standalone bir scope — mevcut hiçbir wire-shape'ine veya komuta dokunmuyorsun, sadece ekleme yapıyorsun.

### 3.2 Otorite (oku)

- `PROJECT_CHARTER.md` Constraint #1
- `AGENTS.md` command surface convention (`{namespace}:{verb}` IPC, Rust `_` underscore)
- `src-tauri/src/commands/workflows.rs` — minimal command pattern (en küçük örnek; `me_get` benzer şekil)
- `Neuron Design/app/data.js` — `data.user.{initials, name}` ve `data.workspace.{name, count}`
- `tasks/refactor.md` §3.3'teki ⑤ kalemi

### 3.3 Allowlist

- `src-tauri/src/commands/me.rs` (yeni)
- `src-tauri/src/commands/mod.rs` (`pub mod me;` ekle)
- `src-tauri/src/lib.rs` (`collect_commands!` listesine `commands::me::me_get` + `// me` namespace yorumu)
- `src-tauri/src/models.rs` (yeni `Me`, `User`, `Workspace` struct'ları **dosyanın sonuna** ekle — mevcut yapıya dokunma)

### 3.4 Görevler

**Görev 1 — `models.rs` (sona ekle):**

```rust
// ---------------------------------------------------------------------
// Me (workspace + user composite)
// ---------------------------------------------------------------------

/// User profile fields surfaced in the Sidebar avatar / settings.
/// Mock parity: `Neuron Design/app/data.js#user`.
/// Week 2 hardcoded; Week 3 sources from a settings table.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub initials: String,
    pub name: String,
}

/// Active workspace metadata. `count` is the number of workflows
/// currently saved (denormalised from `SELECT COUNT(*) FROM workflows`).
/// Mock parity: `Neuron Design/app/data.js#workspace`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub name: String,
    pub count: i64,
}

/// Composite shape returned by `me:get`. Combines `data.user` and
/// `data.workspace` so the Sidebar mounts in one round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub user: User,
    pub workspace: Workspace,
}
```

**Görev 2 — `commands/me.rs` (yeni dosya):**

```rust
//! `me:*` namespace.
//!
//! - `me:get` `()` → `Me`
//!
//! Week 2 returns hardcoded user + workspace count from `workflows`.
//! Week 3 will source the user from a settings table; the wire shape
//! does not change.

use tauri::State;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Me, User, Workspace};

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn me_get(pool: State<'_, DbPool>) -> Result<Me, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflows")
        .fetch_one(pool.inner())
        .await?;
    Ok(Me {
        user: User {
            initials: "ET".into(),
            name: "Efe Taşkıran".into(),
        },
        workspace: Workspace {
            name: "Personal".into(),
            count,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;
    use tauri::Manager as _;

    #[tokio::test]
    async fn me_get_returns_hardcoded_user_and_workspace_count() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w2','PR review')")
            .execute(&pool).await.unwrap();

        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        assert_eq!(me.user.initials, "ET");
        assert_eq!(me.user.name, "Efe Taşkıran");
        assert_eq!(me.workspace.name, "Personal");
        assert_eq!(me.workspace.count, 2);
    }

    #[tokio::test]
    async fn me_get_with_empty_db_returns_zero_count() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        assert_eq!(me.workspace.count, 0);
    }
}
```

**Görev 3 — `commands/mod.rs`:**

`pub mod me;` ekle (alfabetik sırada — agents'tan sonra).

**Görev 4 — `lib.rs::specta_builder_for_export`:**

Mevcut `collect_commands![]` listesinde namespace block yorumlarını izleyerek `// me` block'u ekle ve `commands::me::me_get` ekle. `// agents` ile `// workflows` arası mantıksal olarak `me`'ye en yakın yer; ama sıra fonksiyonel değil estetik — önerim: `// runs` block'undan sonra `// me`'yi koy. Veya alfabetik: agents, mailbox, mcp, **me**, runs, terminal, workflows. Karar serbest, tutarlı olsun.

```rust
            // me
            commands::me::me_get,
```

**Görev 5 — Test (Görev 2'nin altında zaten yazıldı):**

İki test:
- `me_get_returns_hardcoded_user_and_workspace_count`
- `me_get_with_empty_db_returns_zero_count`

### 3.5 Yapma

- Yeni endpoint ailesi (`me:set`, `me:update` vs.) ekleme — yalnızca `me:get`
- `User` veya `Workspace` struct'ına ekstra alan ekleme (mock parity'den fazla)
- Database'e `users` veya `workspaces` tablosu ekleme — Week 2 hardcoded
- `bindings.ts` regen
- Frontend dokunma (`pnpm` çalıştırma)

### 3.6 Acceptance criteria

- [ ] `me_get` komutu kayıtlı (`commands.meGet()` specta wrapper'ı oluşur)
- [ ] `Me`, `User`, `Workspace` mock-shape parity (camelCase: `initials`, `name`, `count`)
- [ ] 2 yeni test geçiyor
- [ ] cargo test --lib full pass
- [ ] cargo check temiz
- [ ] `lib.rs::specta_builder_for_export` listesinde `commands::me::me_get` mevcut

### 3.7 Doğrulama komutları

```bash
cargo check --manifest-path src-tauri/Cargo.toml --tests
cargo test --manifest-path src-tauri/Cargo.toml --lib commands::me
cargo test --manifest-path src-tauri/Cargo.toml --lib   # full regression
```

### 3.8 Çakışma uyarısı

- `models.rs` Ajan A ile paylaşılan. Sen dosyanın **sonuna** Me/User/Workspace ekliyorsun; A `Pane` struct'a (mevcut blok) ek satır + `ApprovalBanner` (terminal block'unda) ekliyor. Diff'leriniz farklı satırlarda.
- `lib.rs` Ajan D ile paylaşılan. Sen `collect_commands!` array'a +1 satır + `// me` yorumu; D setup hook'unun **başına** `tracing_subscriber::fmt::init()` ekliyor. Farklı bloklar.
- `commands/mod.rs` Ajan D ile paylaşılan. Sen `pub mod me;`; D `pub mod util;`. İki ayrı satır, çakışma yok.

### 3.9 Çıktı

§0.3 formatını kullan.

---

## §4. Ajan D — Operasyonel hijyen (kalemler ⑧ + ⑨ + ⑩)

### 4.1 Sen kimsin

Refactor.md §4'teki 3 yeni doğan refaktör fırsatını birleşik olarak ele alacaksın:

- **⑧ tuning.rs** — Magic timing/ölçek sabitlerinin merkezleştirilmesi
- **⑨ tracing adopt + eprintln audit** — `eprintln!` çağrılarını yapılandırılmış log'a geçirme
- **⑩ Compensating-action helper** — `commands/runs.rs:139-149` rollback inline'ının util'e taşınması

Üçü de **davranış-nötr**. Hiçbir IPC shape değişmez, hiçbir test'in assertion'ı değişmez (yalnızca unused warning'ler kaybolur, test count yükselebilir).

### 4.2 Otorite (oku)

- `tasks/refactor.md` §3 (B4 ATLA — başka scope) ve §4 (4 yeni doğan madde, son 3'ünü yapıyorsun)
- `PROJECT_CHARTER.md` Tech stack tablosu — `tracing` zaten "Tracing: Custom OTel-style spans → SQLite" için pinli. `tracing` + `tracing-subscriber` operasyonel logger; OTel span persistence ayrı (WP-W2-07 scope'u, dokunmuyorsun)
- `src-tauri/src/sidecar/agent.rs`, `terminal.rs`, `mcp/client.rs` — mevcut const tanımları + eprintln çağrıları
- `src-tauri/src/db.rs::seed_mcp_servers` — manifest skip eprintln (refactor.md K6 fix'i)
- `src-tauri/src/commands/runs.rs:139-149` — rollback SQL inline

### 4.3 Allowlist

- `src-tauri/src/tuning.rs` (yeni)
- `src-tauri/src/commands/util.rs` (yeni)
- `src-tauri/src/commands/mod.rs` (`pub mod util;` ekle)
- `src-tauri/Cargo.toml` (yalnızca `tracing` + `tracing-subscriber` deps)
- `src-tauri/src/lib.rs` (`pub mod tuning;` + `pub mod util;` değil — `mod commands::util` zaten ihaleli; subscriber init setup hook başında)
- `src-tauri/src/sidecar/agent.rs` (constants → tuning + eprintln → tracing)
- `src-tauri/src/sidecar/terminal.rs` (constants → tuning + eprintln → tracing)
- `src-tauri/src/mcp/client.rs` (constants → tuning + eprintln → tracing)
- `src-tauri/src/db.rs` (yalnızca eprintln → tracing)
- `src-tauri/src/commands/runs.rs` (yalnızca rollback inline → util helper çağrısı)

### 4.4 Görevler

**Görev 1 — `tuning.rs` (yeni dosya):**

```rust
//! Centralised tuning constants — one place to adjust timing,
//! buffer sizes, and timeouts. See `tasks/refactor.md` §4 ("Magic
//! timing/ölçek sabitlerinin dağınıklığı") for rationale.
//!
//! Constants used to live in their respective modules
//! (`sidecar/agent.rs`, `sidecar/terminal.rs`, `mcp/client.rs`).
//! Centralising them here makes it possible to tune the whole
//! runtime profile (low-mem device, high-throughput dev) without
//! file-hunting.

use std::time::Duration;

// ----- Sidecar lifecycle -----

/// How long the LangGraph sidecar gets after a clean `shutdown`
/// frame before we issue `start_kill`. Per `agent.rs` original
/// const.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// How long a terminal pane's `kill_pane` waits for the child to
/// exit before declaring success.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// Polling cadence for `try_wait()` in the per-pane waiter task.
pub const WAIT_POLL: Duration = Duration::from_millis(200);

// ----- Terminal ring buffer -----

/// Hard cap on in-memory ring lines per pane.
pub const RING_BUFFER_CAP: usize = 5_000;

/// Lines dropped from the front when the ring overflows.
pub const RING_BUFFER_DROP: usize = 1_000;

/// Most recent N lines scanned for `awaiting_approval` regex match.
pub const APPROVAL_WINDOW_LINES: usize = 5;

/// PTY chunk read size. 8 KiB matches common pipe-buffer sizes.
pub const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Backpressure cap on pending bytes a single pane's reader can
/// hold before it stops accepting more from the PTY (report.md §L8).
pub const MAX_PENDING_BYTES: usize = 1024 * 1024; // 1 MiB

// ----- MCP client -----

/// One MCP request's deadline (initialize, tools/list, tools/call).
pub const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
```

(Sabit isimleri ve değerleri **mevcut kaynaktan kopyala**. Eğer `mcp/client.rs`'te `MCP_REQUEST_TIMEOUT` farklıysa o değeri kullan, yukarıdaki 30s tahminini değiştir.)

**Görev 2 — `commands/util.rs` (yeni dosya):**

```rust
//! Shared helpers for the command surface.
//!
//! Currently:
//! - [`finalise_run_with`] — atomic run finalisation. Extracted
//!   from `runs.rs:runs_create` rollback (refactor.md §4
//!   "Compensating-action pattern'i inline yazılmış").

use crate::db::DbPool;
use crate::error::AppError;
use crate::time::now_millis;

/// Mark a run as `status` (typically `"error"` for compensating
/// rollback or `"cancelled"` for user-driven cancel) iff it is
/// currently `"running"`. Computes `duration_ms` from the row's
/// `started_at` if not already set.
///
/// Atomic: the `WHERE status = 'running'` guard prevents this from
/// overwriting a sidecar-driven success/error finalisation that
/// already landed.
pub async fn finalise_run_with(
    pool: &DbPool,
    run_id: &str,
    status: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runs SET \
            status = ?, \
            duration_ms = COALESCE(duration_ms, ? - started_at * 1000) \
         WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(now_millis())
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fresh_pool;

    #[tokio::test]
    async fn finalise_run_with_marks_running_run_as_error() {
        let (pool, _dir) = fresh_pool().await;
        sqlx::query(
            "INSERT INTO workflows (id, name) VALUES ('w1','Daily summary'); \
             INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
             VALUES ('r-1','w1','Daily summary',1000,'running')"
        ).execute(&pool).await.unwrap();

        finalise_run_with(&pool, "r-1", "error").await.unwrap();

        let (status, dur): (String, Option<i64>) =
            sqlx::query_as("SELECT status, duration_ms FROM runs WHERE id='r-1'")
                .fetch_one(&pool).await.unwrap();
        assert_eq!(status, "error");
        assert!(dur.is_some());
    }

    #[tokio::test]
    async fn finalise_run_with_does_not_overwrite_completed_run() {
        let (pool, _dir) = fresh_pool().await;
        sqlx::query(
            "INSERT INTO workflows (id, name) VALUES ('w1','Daily summary'); \
             INSERT INTO runs (id, workflow_id, workflow_name, started_at, status, duration_ms) \
             VALUES ('r-1','w1','Daily summary',1000,'success',2400)"
        ).execute(&pool).await.unwrap();

        // try to flip an already-success run to error — must be no-op
        finalise_run_with(&pool, "r-1", "error").await.unwrap();

        let status: String =
            sqlx::query_scalar("SELECT status FROM runs WHERE id='r-1'")
                .fetch_one(&pool).await.unwrap();
        assert_eq!(status, "success", "completed run must not be reverted");
    }
}
```

**Görev 3 — `Cargo.toml` deps:**

```toml
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

**Görev 4 — `lib.rs` subscriber init + module declarations:**

`pub mod tuning;` ekle (mevcut `pub mod` listesine alfabetik).
`commands/mod.rs`'e `pub mod util;` ekle.

Setup hook'un en başına (db::init'ten önce):

```rust
let _ = tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,neuron=info")),
    )
    .try_init();
```

`try_init` çünkü test ortamında çoklu init'i tolere etsin (panic etmez).

**Görev 5 — Const taşımaları:**

`sidecar/agent.rs`:
- `const SHUTDOWN_GRACE: Duration = ...` → sil; `use crate::tuning::SHUTDOWN_GRACE;`

`sidecar/terminal.rs`:
- `const RING_BUFFER_CAP / RING_BUFFER_DROP / APPROVAL_WINDOW_LINES / READ_CHUNK_BYTES / KILL_GRACE / WAIT_POLL / MAX_PENDING_BYTES` → sil; `use crate::tuning::*;` veya per-name

`mcp/client.rs`:
- Mevcut request timeout sabiti (varsa) → sil; `use crate::tuning::MCP_REQUEST_TIMEOUT;`

**Görev 6 — eprintln → tracing audit:**

Tüm `eprintln!` çağrılarını uygun seviyeye çevir. Genel rehber:

| Eski | Yeni |
|---|---|
| `eprintln!("[setup] LangGraph sidecar unavailable: {e} ...");` | `tracing::warn!(error = %e, "LangGraph sidecar unavailable; run uv sync to install");` |
| `eprintln!("[mcp:seed] skipping bundled manifest `{}`: {}", file, err);` | `tracing::warn!(file_key = %file, error = %err, "skipping bundled MCP manifest");` |
| `eprintln!("[sidecar] frame error: {e}");` | `tracing::error!(error = %e, "sidecar frame error");` |
| `eprintln!("[sidecar] stdout closed; read loop exiting");` | `tracing::info!("sidecar stdout closed; read loop exiting");` |
| `eprintln!("[sidecar] decode error: {e}; body: {:?}", body);` | `tracing::warn!(error = %e, body = ?body, "sidecar frame decode error");` |
| `eprintln!("[sidecar] handle_event: {e}");` | `tracing::error!(error = %e, "sidecar handle_event failed");` |
| `eprintln!("[sidecar] sidecar reported error: {}", msg);` | `tracing::warn!(message = %msg, "sidecar reported non-fatal error");` |
| `eprintln!("[sidecar] agent runtime ready");` | `tracing::info!("LangGraph agent runtime ready");` |
| `eprintln!("[sidecar] run {run_id} ended in {status}: {msg}");` | `tracing::info!(run_id = %run_id, status = %status, error = %msg, "run completed");` |
| `eprintln!("[terminal:{pane_id}] flush failed: {e}");` | `tracing::error!(pane_id = %pane_id, error = %e, "terminal ring flush failed");` |
| `eprintln!("[terminal:{pane_id}] shutdown_all ring flush failed: {e}");` | `tracing::error!(pane_id = %pane_id, error = %e, "shutdown_all ring flush failed");` |
| `eprintln!("[terminal:{pane_id}] shutdown_all finalise failed: {e}");` | `tracing::error!(pane_id = %pane_id, error = %e, "shutdown_all finalise failed");` |
| `eprintln!` MCP version drift | `tracing::warn!(expected = %exp, got = %got, "MCP protocolVersion mismatch");` |

**Test'lerdeki `eprintln!`'lere DOKUNMA** — `#[cfg(test)]` block'ları kapsam dışı.

**Görev 7 — `commands/runs.rs` rollback refactor:**

Mevcut:
```rust
// inline UPDATE rollback
sqlx::query("UPDATE runs SET status='error', duration_ms = ? WHERE id = ? AND status = 'running'")
    .bind(now_ms - started_ms).bind(&run_id).execute(pool).await?;
```

Yeni:
```rust
crate::commands::util::finalise_run_with(pool, &run_id, "error").await?;
```

İki test var: `runs_create_falls_back_to_error_on_sidecar_failure` (veya benzeri). Davranış aynı — geçmesi gerek.

### 4.5 Yapma

- IPC shape değişikliği — yeni alan, yeni komut, yeni event
- LangGraph veya MCP protokol değişikliği
- Test'lerin assertion'larını değiştirme — sadece eprintln'leri değiştir
- `cfg(test)` bloklarındaki eprintln'leri çevirme (test çıktısı için kasıtlı stderr)
- Dependency upgrade — yalnızca tracing + tracing-subscriber'ı **ekle**
- `bindings.ts` regen (zaten regen olmaz çünkü shape değişmedi)
- Davranış değişikliği — özellikle `finalise_run_with`'in atomicity invariant'ını koru (`WHERE status = 'running'` guard)

### 4.6 Acceptance criteria

- [ ] `tuning.rs` 8+ const, hepsi belgeli
- [ ] `commands/util.rs::finalise_run_with` mevcut, 2 test geçiyor
- [ ] `commands/runs.rs` rollback artık helper'ı çağırıyor
- [ ] tracing dep eklendi, subscriber init'lendi
- [ ] **Aktif** `eprintln!` çağrıları (test dışı) `tracing::*`'a çevrildi — `grep -rn 'eprintln!' src-tauri/src --include='*.rs'` test bloklarındaki çağrılar dışında 0 sonuç
- [ ] cargo test --lib full pass (regression yok — davranış değişmedi)
- [ ] cargo check temiz, 0 lint warning (yeni warning oluşursa fix)
- [ ] `cargo run --bin export-bindings` halen çalışıyor — bindings.ts diff'i yok (shape değişmedi)

### 4.7 Doğrulama komutları

```bash
cargo check --manifest-path src-tauri/Cargo.toml --tests
cargo test --manifest-path src-tauri/Cargo.toml --lib commands::util
cargo test --manifest-path src-tauri/Cargo.toml --lib commands::runs
cargo test --manifest-path src-tauri/Cargo.toml --lib   # full regression
cargo run --manifest-path src-tauri/Cargo.toml --bin export-bindings
git diff app/src/lib/bindings.ts   # boş olmalı
```

### 4.8 Çakışma uyarısı

Sen **paylaşılan dosyaların hepsine dokunuyorsun**. Worktree dispatch'te B/C/A sırasıyla bittikten sonra sen başlatılırsan A/B/C'nin yeni eklediği eprintln'leri de yakalarsın (Ajan A'nın `terminal.rs`'e eklediği reader-side eprintln'ler, Ajan B'nin `data.js`/`db.rs` eprintln'leri yok ama olası, Ajan C'nin yok).

**Eğer paralel başlatılıyorsan:** worktree isolation altında kendi diff'in temiz, orchestrator merge'inde A/B/C tarafının eprintln'leri sende olmaz — orchestrator post-merge tek bir audit pass'inde temizler. Brief'in disiplini korumak için A/B/C bittikten sonra başlatılmak en güvenlisi.

### 4.9 Çıktı

§0.3 formatını kullan. **Ek handoff:** Tracing subscriber init'i ortam değişkeni `RUST_LOG`'a saygı gösteriyor; orchestrator'a "dev'de `RUST_LOG=neuron=debug` set edip log seviyesini test etmesi" hatırlatması.

---

## §5. Orchestrator merge protokolü

### 5.1 Genel akış

4 ajan tamamlandığında orchestrator (parent session) şu adımları izler:

```
1. Sıralı worktree merge:    B → C → A → D (ana branch'e fast-forward)
2. bindings.ts regen:         cargo run --bin export-bindings
3. Final regression:           cargo test --lib + pnpm typecheck + pnpm test + pnpm lint
4. Manuel review tarama:       allowlist disiplini, mock parity, known caveats
5. Commit yapısı kararı:       4 atomic commit veya 1 paket commit
6. refactor.md + AGENT_LOG.md güncelleme
```

### 5.2 Sıralı merge sırası ve sebebi

- **B önce:** En az çakışma, en hızlı doğrulama. Mock realign + manifest stub'ları ana branch'e oturursa A/B/C için test fixture'ı haline gelir.
- **C ikinci:** Standalone — `commands/me.rs` yeni dosya, `models.rs` sonu, `lib.rs` +1 satır. B'nin test fixture'ından bağımsız.
- **A üçüncü:** Pane domain. `models.rs::Pane`'e dokunduğu için C'den sonra (sırada `models.rs` C ile çakışmaz ama context tutarlılığı).
- **D son:** Operasyonel hijyen tüm A/B/C eprintln'lerini kapsayabilsin.

Eğer worktree isolation kullanılmıyor ve seri dispatch yapılıyorsa, sıra zaten zorunludur. Worktree paralel dispatch'te de orchestrator merge'i bu sırayla yapar.

### 5.3 bindings.ts regen beklenen değişiklikleri

```bash
cargo run --manifest-path src-tauri/Cargo.toml --bin export-bindings
git diff --stat app/src/lib/bindings.ts
```

Beklenen:
- `Pane` struct'a 5 yeni TS field (`tokensIn`, `tokensOut`, `costUsd`, `uptime`, `approval`)
- `ApprovalBanner` yeni TS type
- `Me`, `User`, `Workspace` yeni TS types
- `commands.meGet` yeni typed wrapper
- Önceki tur `MailboxEntry`/`MailboxEntryInput` `from`/`to` rename intact (regression yok)

Tahmini diff: ~30–50 satır.

### 5.4 Final regression

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib   # ≥ 100 passed (95 base + ~5 yeni)
cargo check --manifest-path src-tauri/Cargo.toml --tests
pnpm typecheck
pnpm test --run
pnpm lint
```

Hepsi yeşilse paket merge için hazır.

### 5.5 Allowlist disiplin doğrulaması

Her ajan worktree'sinde `git diff --stat` çıktısını brief'in allowlist'iyle karşılaştır. Allowlist dışına çıkmış dosya varsa:
- Trivial ise (örn. comment fix) handoff'a not yaz
- Önemliyse rollback ve ajanı re-dispatch (brief'i daraltarak)

### 5.6 Commit yapısı önerisi

**Atomic, 4 commit:**

```
feat(panes): wire approval banner + nullable derived fields (③+④)
feat(mcp): expand catalog to 12 servers per mock (⑥+⑦)
feat(me): add me:get command for user/workspace (⑤)
refactor: tuning module + tracing adopt + finalise helper (⑧+⑨+⑩)
```

Bindings.ts regen'i son atomic commit'e dahil et veya ayrı `chore: regen bindings.ts` commit'i — atomic'lik zarar görür ama F1 drift guard kuralı korunur.

### 5.7 refactor.md güncelleme

§1 tablosunda şu maddeleri ✅'e taşı:
- (Ajan A): yeni "③ Pane shape mock-shape parity" madde — refactor.md formatına uygun ekleme veya §3 listesine ekle
- (Ajan B): G2'nin önemli kısmı — manifest catalog tamam; "G2 Filesystem dışı UX" Week 3'e kayar
- (Ajan C): yeni "⑤ me:get komutu"
- (Ajan D): ⑧ tuning, ⑨ tracing, ⑩ compensating helper — §4 "yeni doğan" listesinden ✅ kategorisine taşı

### 5.8 AGENT_LOG.md girdisi

Tek bir `2026-04-29 — 4-agent followup paketi` girdisi (precedent: `2026-04-28T17:30:54Z docs/review-2026-04-28 completed`):

```markdown
## 2026-04-29TXX:XX:XXZ 4-agent followup completed
- sub-agents: A (panes), B (mcp catalog), C (me:get), D (operasyonel hijyen)
- files changed: N total (X yeni, Y modified)
- migrations added: 0003_panes_approval.sql
- new commands: me:get
- new models: ApprovalBanner, Me, User, Workspace
- mcp catalog: 6 → 12
- tracing adopted, eprintln audit complete
- acceptance: ✅ all four briefs pass
- final regression: cargo test --lib X passed; pnpm typecheck/test/lint exit 0
- bindings.ts regen: yes, ~N satır diff
- branch: main (local; not pushed)
- next: WP-W2-07 (tracing persistence) veya WP-W2-08 (frontend wiring)
```

### 5.9 Bilinen handoff noktaları

Her ajan brief'inin §X.9 "handoff to orchestrator" satırından gelen notlar:

- **Ajan B:** `default_installed` mismatch (mock 3 sunucu pre-installed vs backend hepsi 0) Week 3 G2 ile çözülecek; refactor.md'de ⏳ olarak güncel kalır
- **Ajan D:** `RUST_LOG=neuron=debug` ile dev test yapılması; production bundle'da log seviyesi default WARN
- **Genel:** Bu paket `bindings.ts` shape'ini büyütür — frontend WP-W2-08'de yeni hook'lar yazılırken bu yeni alanları (Pane.tokensIn vs.) consume edecek

---

## §6. Brief sonu — orchestrator için kontrol listesi

- [ ] 4 ajan dispatch edildi (sıra: B → C → A → D, veya paralel worktree)
- [ ] Her ajan'ın §X.6 acceptance criteria item'ları geçti (ajan kendi raporladı)
- [ ] Her ajan'ın §X.7 doğrulama komutları çıktısı temiz
- [ ] Her ajan worktree'si ana branch'e merge edildi (sırada)
- [ ] `cargo run --bin export-bindings` çalıştırıldı, bindings.ts diff incelendi
- [ ] `cargo test --lib` full pass
- [ ] `pnpm typecheck` + `pnpm test --run` + `pnpm lint` exit 0
- [ ] Allowlist disiplini doğrulandı (her ajan'ın diff'i brief'iyle uyumlu)
- [ ] `tasks/refactor.md` güncellendi
- [ ] `AGENT_LOG.md` girdisi yazıldı
- [ ] Commit yapısı kararı (atomic 4 vs paket 1) — kullanıcıya sor

---

**Bu briefler 2026-04-29 itibarıyla geçerlidir.** Working tree'de uncommit değişiklikler varsa ajan brief'lerine başlamadan önce orchestrator'a danış (kendi worktree'sinde çalışman gerekebilir).
