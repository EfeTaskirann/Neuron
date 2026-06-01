# Neuron — Otonom İyileştirme Backlog'u

**Tohumlama:** 2026-05-31 (ilk inceleme). Her tur bu dosyayı okur, çözdüğünü işaretler, yeni doğanı ekler.
**Statü kodları:** ☐ açık · ◐ kısmen · ☑ çözüldü · ⏸ bloklu (büyük/parçalı PR) · 🔥 hot-zone (audit-only, W6 aktif)

> Öncelik: kırık tab / kırmızı kapı > Tier 1 > Tier 2 > Tier 3. Mod: stable→apply, dirty→audit. Her madde: **etki · risk · kanıt**.

---

## Tier 1 — Tab fonksiyonel sağlık + UI/UX (kullanıcının açık önceliği)

### T1-01 ◐ 9-tab render smoke testi tamamla
- **Etki:** "her tab çalışıyor mu" garantisini teste bağlar (kullanıcının birincil isteği).
- **Tab envanteri** (`app/src/App.tsx` NAV): `canvas`(Workflow), `terminal`, `terminal-swarm`, `swarm`, `agents`, `runs`, `mcp`, `routing-log`, `settings` + `RunInspector` (canvas yan panel).
- **Mevcut kapsam (2026-05-31_1400 turunda haritalandı):**
  - `App.test.tsx` (hot-zone) shell üzerinden tıklayarak: canvas, terminal, agents, runs, mcp, RunInspector — yalnız populated render (loading/empty yok).
  - `SwarmRoute.test.tsx` (stable): swarm — bağımsız.
  - **YENİ bu tur (apply):** `AgentsRoute.test.tsx` + `MCPRoute.test.tsx` (stable) — bağımsız, izole; loading + populated + empty-state üçlüsü (App.test.tsx'in atladığı durumlar).
- **Kalan boşluk → hepsi hot-zone (dirty W6) → AUDIT-only, W6 commit'lenince apply:** `terminal-swarm` (TerminalSwarm.tsx), `routing-log` (RoutingLogRoute.tsx), `settings` (SettingsRoute.tsx) — hiçbirinin render-smoke'u yok.
- **Risk:** düşük (test ekleme). **Mod:** apply (stable route'lar) / audit (dirty route'lar).

### T1-02 ☐ ErrorBoundary'ye düşen route taraması
- **Etki:** bir tab açılışta patlıyorsa kullanıcı "Couldn't load …" kartı görür; bunları sıfıra indir.
- **Yapılacak:** her route'u mount edip ErrorBoundary fallback'ine düşeni tespit eden test; düşen varsa kök neden + düzeltme.
- **Risk:** orta (gerçek bug çıkabilir). **Mod:** apply (stable route) / audit (TerminalSwarm dirty ise).

### T1-03 ☐ Stale App.tsx başlık yorumu + "RouteStub coming soon"
- **Kanıt:** `app/src/App.tsx:1-5` "Routes are stubs (coming soon)" diyor ama route'lar gerçek bileşenlere bağlı. `RouteStub`/`route-stub` default-case ölü olabilir.
- **Yapılacak:** yorumu güncelle; `RouteStub` hâlâ erişilebilir mi kontrol et, değilse sil.
- **Risk:** düşük. **Mod:** apply.

### T1-04 ☐ Loading / empty / error durum denetimi (UX)
- **Etki:** TanStack Query hook'ları olan her route'ta `isLoading`/`isError`/boş-veri durumları tutarlı mı? Eksikler kullanıcıya boş/çakık ekran gösterir.
- **Yapılacak:** route bazında denetle; eksik state'leri tutarlı bir desene (skeleton/empty card) bağla.
- **Risk:** düşük-orta. **Mod:** apply.

### T1-05 ☐ OKLCH + design-system parity denetimi
- **Etki:** Charter #4 (OKLCH only). Yeni CSS'te hex/HSL kaçağı = ihlal.
- **Yapılacak:** `app/src/styles/*.css` (`app.css`, `swarm-term.css`, `terminal.css`) içinde hex/HSL tara; `design-system-spec.md`'ye göre token sapmalarını düzelt.
- **Risk:** düşük. **Mod:** apply (SVG içindeki legacy hex hariç — Charter izinli).

### T1-06 ☐ Ölü CSS / kullanılmayan bileşen
- **Kanıt:** `RoutingOverlay.tsx` silinmiş (git status) — ona ait CSS/import kaldı mı? Yeni `ToastHost`, `SwarmHierarchy` eklenmiş — stil eşleşmesi tam mı?
- **Yapılacak:** kullanılmayan CSS sınıfı / export / import tara ve sil.
- **Risk:** düşük. **Mod:** apply.

### T1-07 ☐ Erişilebilirlik (a11y) temel geçiş
- **Etki:** UX kolaylığı. Sidebar `li` tıklanabilir ama `role`/klavye yok (`App.tsx:93-103`); arama input'u `⌘K` kbd var ama bağlı kısayol var mı?
- **Yapılacak:** tıklanabilir `li`/`div`'lere rol+klavye, ikon-only butonlara `aria-label`.
- **Risk:** düşük. **Mod:** apply.

---

## Tier 2 — Frontend refactor (stable zone)

### T2-01 ☐ `app/src/routes/Terminal.tsx` (654L) bölünmesi
- **Etki:** en büyük frontend route; xterm kurulum + pane state + render bir arada.
- **Yapılacak:** xterm/PTY köprü mantığını hook'a (`useTerminalPane`), sunumu alt-bileşene ayır. Davranış birebir korunur.
- **Risk:** orta (xterm yan etkileri). **Mod:** apply — **ama** snapshot'ta dirty ise (W6) → 🔥 audit.

### T2-02 ☐ `app/src/components/OrchestratorChatPanel.tsx` (322L)
- **Etki:** test'i (359L) kendisinden büyük → karmaşık. Sunum/efekt ayrımı.
- **Risk:** orta. **Mod:** apply.

### T2-03 ☐ Hook DRY denetimi (`app/src/hooks/`)
- **Kanıt:** çok sayıda yeni hook (`useRoutingEvents` 302L, `useAppearance`, `useActiveProject`, `useClaudeUpdateProgress`, `useTerminalSwarmSession`). Tekrarlı invoke/listen/cleanup desenleri merkezleşebilir.
- **Risk:** düşük-orta. **Mod:** apply (dirty olanlar audit).

---

## Tier 3 — Backend refactor (stable zone) + teknik borç

### T3-01 ⏸ `src-tauri/src/commands/swarm.rs` (2878L) namespace bölünmesi
- **Etki:** en büyük Rust dosyası; tek komut namespace'i dev olmuş.
- **Yapılacak:** alt-modüllere böl (ör. `commands/swarm/{jobs,agents,dispatch,query}.rs`). `collect_commands!` listesi korunur.
- **Risk:** yüksek (büyük diff, IPC yüzeyi). **Mod:** ⏸ parçalı PR — turda yalnız **audit + bölünme planı**.

### T3-02 ⏸ `swarm/projector.rs` (2430L) · `brain.rs` (2013L) · `agent_dispatcher.rs` (1967L)
- **Etki:** swarm çekirdeği; her biri tek dosyada çok sorumluluk.
- **Risk:** yüksek. **Mod:** ⏸ audit + parçalı plan; küçük, izole saf-fonksiyon çıkarımları apply edilebilir.

### T3-03 ☐ `src-tauri/src/sidecar/terminal.rs` (2049L) — düşük-riskli çıkarımlar
- **Etki:** PTY supervisor + ring buffer + ANSI strip bir arada.
- **Yapılacak:** saf yardımcılar (ANSI strip, ring-buffer trim, tuning sabitleri) ayrı modüle; davranış korunur.
- **Risk:** orta. **Mod:** apply (saf fonksiyonlar) / audit (PTY yaşam döngüsü).

### T3-04 ☐ refactor-v1 ertelenenleri — **hâlâ açık mı doğrula**
Repo W2'den W6'ya ilerledi; bu maddeler kapanmış olabilir. Her turda önce **var/yok teyidi**:
- `Supervisor` trait soyutlaması (C1) — `agent.rs` + `terminal.rs` ortak supervisor. ⏸ üçüncü sidecar gelince.
- MCP session pool + pending request map (C5) — `mcp/client.rs` (609L). ⏸ ayrı WP.
- Ortak `Status` enum (C2) — terminal vs run status ayrı enum'lar.
- Seeds modülü konsolidasyonu (D2).
- Capabilities daraltma (E1) — `tauri.conf.json` komut yüzeyi sabitlenince.
- Repo-level pre-commit hook (E2) — Co-Author trailer + gate; `core.hooksPath`/Husky ADR'ı ile.
- **Mod:** her biri audit + (küçükse) apply.

### T3-05 ☐ `tracing` / yapılandırılmış log denetimi
- **Etki:** refactor-v1 §⑨ `tracing` adopt etmişti; W3-W6 yeni kodda kaçak `eprintln!`/`println!` var mı?
- **Risk:** düşük. **Mod:** apply.

---

## 🔥 Hot zone — W6 aktif (snapshot'ta dirty → AUDIT-ONLY, patch yok)

> Bu alan ~17 gündür commit edilmemiş W6 (PTY router → dosya-tabanlı JSON inbox bridge) işidir. **Patch üretme** — yalnız bulgu + öneri. Kullanıcı W6'yı commit edince stable zone'a düşer.

- 🔥 `src-tauri/src/swarm_term/bridge.rs` (987L), `lifecycle.rs` (744L), `session.rs`, `persona.rs`, `home_isolation.rs`, `hierarchy.rs`
- 🔥 `app/src/routes/TerminalSwarm.tsx` (511L), `app/src/hooks/useTerminalSwarmSession.ts`, `useRoutingEvents.ts`, `app/src/styles/swarm-term.css`
- 🔥 9 persona doc'u (`src-tauri/src/swarm/agents/term/*.md`)
- **W6-02 izi** (`WP-W6-01-bridge-routing.md:62`): `useRoutingEvents.ts:6` stale yorum, design-parity audit, commit hijyeni. → audit notu.
- 🔥 **App.tsx:1-5 stale başlık yorumu (T1-03 doğrulandı):** "Routes are stubs ('coming soon')" diyor; oysa `App.tsx:12-21` 9 route'un tamamını gerçek bileşene bağlıyor. Stub kalmadı. App.tsx dirty → audit. W6 commit'lenince yorumu güncelle.
- 🔥 **App.test.tsx:234 stale assertion:** "renders the sidebar with all **7** nav items" — `NAV` artık **9** giriş (canvas, terminal, terminal-swarm, swarm, agents, runs, mcp, routing-log, settings). Test muhtemelen alt-küme dolaşıyor; sayı etiketi yanıltıcı. App.test.tsx dirty → audit.
- 🔥 **A11y (T1-07) doğrulandı:** `App.tsx:92-103` sidebar `li` yalnız `onClick` — `role`/`tabIndex`/klavye yok; `App.tsx:76` `.sb-brand` `role="button"` ama klavye handler'ı yok. Dirty → audit.

---

## Çözülenler (turlar buraya taşır)

*(henüz yok — ilk tur sonrası dolacak)*
