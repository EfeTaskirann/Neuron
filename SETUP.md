# Neuron — Development Setup

Bu doküman Neuron'u **yeni bir makineden** çalıştırmak için
gereken adımları sıralar. Mevcut bir clone üzerinde sadece `git
pull && pnpm install` yeterli; aşağıdaki tam kurulum **temiz bir
makinede** veya **bağımlılıkları sıfırlamak istediğinde**
kullanılır.

> Repo: <https://github.com/EfeTaskirann/Neuron>
> Default branch: `main`
> Git user: EfeTaskirann · efe.taskiran63@gmail.com

---

## 1. Önkoşullar

| Araç | Sürüm | Neden |
|---|---|---|
| Windows 10/11 | desteklenen | hedef platform; Linux/macOS de çalışır ama smoke testler Windows'ta yapıldı |
| Node.js | ≥ 20 | `app/` paketi için — Vite 5 + Vitest 2.x |
| pnpm | 10.33.2 (pinned) | workspace yöneticisi (`packageManager` `package.json`'da pinned) |
| Rust | 1.95.0 stable | `src-tauri/` Tauri 2 + sqlx + tokio |
| Tauri 2 platform deps | OS bazlı | Windows için MSVC + WebView2; bkz. <https://tauri.app/start/prerequisites/> |
| `claude` CLI | latest | Anthropic'in resmi Claude Code CLI'ı; Pro/Max aboneliği **zorunlu** |
| Python 3.11+ + uv | opsiyonel | sadece LangGraph "Daily summary" sidecar için; swarm runtime onsuz da çalışır |

### Hızlı yol (Windows)

```powershell
# Node.js (LTS) — ya nodejs.org installer ya da winget
winget install OpenJS.NodeJS.LTS

# pnpm — Node geldiyse:
npm i -g pnpm@10.33.2

# Rust — rustup-init
winget install Rustlang.Rustup
rustup default stable

# Tauri prereqs — Microsoft C++ Build Tools (varsa atla)
# https://visualstudio.microsoft.com/visual-cpp-build-tools/
# WebView2 zaten Win10/11'de gelir.

# Claude Code CLI
npm i -g @anthropic-ai/claude-code
claude login
# Tarayıcı açılır → Claude.ai hesabınla giriş yap → Pro/Max paketin
# olduğunu doğrula. Bu adım kritik — swarm runtime OAuth session'ını
# kullanır, ayrı API key gerektirmez.
```

### Linux/macOS notu

Yukarıdaki Windows komutlarını platform muadilleriyle değiştir
(`brew install`, `apt install`, vs). Tauri prereqs için resmi sayfa:
<https://tauri.app/start/prerequisites/>.

---

## 2. Repo'yu klonla

```bash
git clone https://github.com/EfeTaskirann/Neuron.git
cd Neuron
pnpm install   # workspace install — kök ve app/ bağımlılıklarını bir kerede çeker
```

İlk `pnpm install` ~30-60 saniye sürer. node_modules `.gitignore`'da;
commit edilmemiş.

---

## 3. Sağlık kontrolü (önemli — bağımlılık eksikliğini erken yakalar)

```powershell
# Rust derleme + test
cd src-tauri
cargo build --lib
cargo test --lib
# 435 / 0 / 14 ignored beklenir (W4 sonrası snapshot)

cd ..

# Frontend test + type check + lint
pnpm typecheck
pnpm lint
pnpm test --run
# 65 / 0 beklenir

# Tauri specta bindings drift kontrolü
pnpm gen:bindings:check
# Exit 0 (uncommitted bindings.ts varsa exit 1; o durumda commit at)
```

Tüm bu komutlar yeşilse setup tamam. Bir hata varsa:

- `cargo build` `LNK1181 oldnames.lib`/`legacy_stdio_definitions.lib`
  hatası verirse → MSVC + Windows SDK kurulumu eksik. Visual Studio
  Build Tools 2022'yi tam yükle (Desktop development with C++ workload).
- `pnpm install` `pnpm v10` istiyorsa → `npm i -g pnpm@10.33.2`
- `claude` not found → `npm i -g @anthropic-ai/claude-code`

---

## 4. Çalıştırma

### Dev modu (canlı reload Tauri pencere)

```powershell
pnpm tauri dev
```

İlk run ~1-2 dakika derler (Rust + frontend bundle). Sonraki
run'lar saniyeler. Pencere açıldığında:

- **Sol kenar çubuğu**: Workflow / Terminal / **Swarm** / Agents / Runs / MCP / Settings
- **Swarm sekmesi**: Default view = `Live grid` — 9 agent panesi.
  Tab switcher ile `Recent jobs` (eski chat panel) görünümüne
  geçilebilir.

### Production build

```powershell
pnpm tauri build
```

`src-tauri/target/release/bundle/` altında installer üretir
(.msi / .exe Windows için).

---

## 5. Repo structure quick-tour

```
Neuron/
├── PROJECT_CHARTER.md      ← ilk önce bunu oku — hard constraints
├── AGENTS.md               ← sub-agent contract + dispatch protocol
├── AGENT_LOG.md            ← her commit'in retrospective journal'ı
├── docs/
│   ├── adr/                ← Architecture Decision Records
│   └── work-packages/      ← WP-W2-* / WP-W3-* / WP-W4-* contract dosyaları
│       ├── WP-W3-overview.md
│       └── WP-W4-overview.md
├── src-tauri/              ← Rust backend (Tauri 2 + sqlx + swarm runtime)
│   └── src/
│       ├── commands/       ← Tauri IPC namespaces (mailbox, mcp, swarm, …)
│       ├── swarm/          ← 9-agent multi-agent runtime
│       │   ├── agents/     ← 9 .md persona files (orchestrator/coordinator/scout/…)
│       │   ├── persistent_session.rs   ← W4-01
│       │   ├── agent_registry.rs       ← W4-02 + W4-06
│       │   ├── help_request.rs         ← W4-05
│       │   └── coordinator/            ← FSM
│       └── lib.rs          ← Tauri setup + IPC wiring
└── app/                    ← React 18 + Vite + TanStack Query frontend
    └── src/
        ├── components/     ← AgentPane / SwarmAgentGrid / OrchestratorChatPanel / …
        ├── hooks/          ← useAgentEvents / useAgentStatuses / useSwarmJob / …
        ├── routes/         ← SwarmRoute / Canvas / RunInspector / …
        └── lib/
            └── bindings.ts ← cargo-üretilmiş typed Tauri command bridge
```

---

## 6. İlk okuma sırası (yeni bir makineden devam ediyorsan)

1. **`PROJECT_CHARTER.md`** — proje kurallarının özeti
2. **`AGENT_LOG.md`** baş kısmı — son birkaç gün ne yapıldı
3. **`docs/work-packages/WP-W4-overview.md`** — son completed iş paketi (persistent visible swarm runtime)
4. **`AGENTS.md`** — sub-agent dispatch protokolü (eğer sub-agent çağıracaksan)

---

## 7. Önemli env değişkenleri

| Var | Default | Etkisi |
|---|---|---|
| `NEURON_CLAUDE_BIN` | (auto-resolve) | `claude` binary path override |
| `NEURON_SWARM_STAGE_TIMEOUT_SEC` | 60 (180 testlerde) | FSM her stage'i için timeout |
| `NEURON_SWARM_AGENT_TURN_CAP` | 200 | persistent session turn cap (üstüne respawn) |
| `NEURON_OTEL_ENDPOINT` | (boş) | OTLP collector endpoint; boşsa export döngüsü çalışmaz |
| `RUST_LOG` | `warn,neuron=info` | tracing seviyesi (`debug` için override) |

---

## 8. Bilinen tuzaklar

1. **Bundled persona değişikliği** (`src-tauri/src/swarm/agents/*.md`):
   `include_dir!` macro cargo'ya rebuild trigger sağlamıyor. `.md`
   düzenledikten sonra `src-tauri/src/swarm/profile.rs`'in mtime'ını
   güncelle:
   ```powershell
   (Get-Item 'src-tauri/src/swarm/profile.rs').LastWriteTime = Get-Date
   ```
   Sonra `cargo build` tekrar çalıştır.

2. **Subscription OAuth kayıp**: `claude` CLI auth oturumu süresi
   dolarsa swarm subprocess'leri `error_during_execution: OAuth
   expired` ile düşer. Çözüm: `claude logout && claude login`.

3. **Windows AV cold cache**: ilk `claude.cmd` spawn'ı (uygulama
   açıldıktan sonra) ~30-60s sürer; AV taraması nedeniyle. Sonraki
   spawn'lar hızlı. Persistent session pattern (W4) bu cold-start'ı
   workspace başına bir kere'ye düşürür.

4. **`gen:bindings:check` exit 1 — pre-commit**: bindings'leri
   regen ettiysen ama commit etmediysen, check exit 1 verir. Bu
   beklenen — sadece `git add app/src/lib/bindings.ts && git commit`
   yapıp tekrar dene.

---

## 9. Mevcut durum (2026-05-07 itibarıyla)

- **Week 2 + 3 + 4 shipped.** WP-W4 (persistent visible swarm
  runtime) yeni kapatıldı.
- **9 agent ekibi** end-to-end çalışıyor: Orchestrator (chat brain)
  → Coordinator (routing brain) → 7 specialist (Scout / Planner /
  Backend-Builder / Frontend-Builder / Backend-Reviewer /
  Frontend-Reviewer / Integration-Tester).
- **3×3 live grid UI** Swarm sekmesinde default view.
- **Test counts:** Rust 435/0/14 (12 ignored real-claude smokes
  + 2 transport smokes), Frontend 65/0.
- **Açık W3 backlog:** W3-04 (LangGraph cancel — deferred),
  W3-05 (approval UI), W3-08 (multi-workflow editor), W3-09
  (capabilities + E2E), W3-10 (Python embed). Hepsi swarm
  runtime'ından bağımsız; istersen sıraya bakmaksızın herhangi
  birinden devam edebilirsin.

---

## 10. Yardım

- AGENTS.md § "Sub-agent contract" — sub-agent dispatch nasıl yapılır
- `docs/work-packages/` — geçmiş WP'lerin nasıl yazıldığı şablon
- Tauri 2 docs: <https://tauri.app/start/>
- Claude Code CLI docs: <https://docs.anthropic.com/en/docs/claude-code/overview>
