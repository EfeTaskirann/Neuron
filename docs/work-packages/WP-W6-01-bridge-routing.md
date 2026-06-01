# WP-W6-01 — Swarm-term Routing Refactor: PTY-marker → File-system JSON Inbox Bridge

## Konteks

Önceki mimari: claude REPL pane'leri arasında mesajlaşma için PTY akışına bracketed marker enjekte edip okuyan router (src-tauri/src/swarm_term/router.rs ~1138L) + marker parser (marker.rs ~531L). Sorunlar: claude'un mid-stream cursor jump'ları marker body'sini bölüyor (63a248a fix), HOME izolasyonu bozulabiliyor (de4b739, 17f1f74), 4 inter-agent comms breakdown root identified (d4c656a). Persona dokümanları a965cad'de 'Proje konteksi' bloğu eklenerek hizalandı ama core router hâlâ kırılgan.

Yeni mimari: dosya-tabanlı IPC. Her ajan kendi inbox'una atomik JSON yazar (`Write` tool), backend (`bridge.rs`) inotify/poll ile dosyayı görür, hedef pane'e bracketed-paste eder, dosyayı `processed/` altına taşır. İzin verilmeyen hedefler `rejected/` altına gider. 4-state lifecycle (alındı/tamam/belirsiz/hata) ve builder/reviewer token'ları (DONE/APPROVED/CHANGES_NEEDED/TASK_DONE) `lifecycle.rs` içinde state machine olarak modellenir.

## Kapsam

Silinen: src-tauri/src/swarm_term/router.rs (~1138L), src-tauri/src/swarm_term/marker.rs (~531L), kök dizindeki AGENT_LOG.md (1733L iz).

Yeni: src-tauri/src/swarm_term/bridge.rs (~1023L), src-tauri/src/swarm_term/lifecycle.rs (~733L), app/src/hooks/useRoutingEvents.ts (260L), app/src/routes/RoutingLogRoute.tsx (175L).

Değişen (büyük): src-tauri/src/swarm_term/session.rs (+779), src-tauri/src/swarm_term/hierarchy.rs (+270), src-tauri/src/swarm_term/mod.rs (re-export), src-tauri/src/commands/swarm_term.rs (+174), src-tauri/src/sidecar/terminal.rs (+74), app/src/routes/TerminalSwarm.tsx (+484), app/src/styles/swarm-term.css (+416), app/src/hooks/useTerminalSwarmSession.ts (+90), app/src/lib/bindings.ts (+29 Tauri komut binding'leri).

Doküman: 9 term-personası (orchestrator, coordinator, planner, scout, backend/frontend builder, backend/frontend reviewer, integration-tester) yeniden yazıldı — Write-tool JSON şeması + 4-state lifecycle + Builder/Reviewer token'ları + 'Proje konteksi' bloğu (a965cad).

## IPC JSON Şeması

Yol: `.bridgespace/<session>/inbox/<hedef_ajan>/<msg_id>.json`. `<msg_id>` = unix-epoch-ms + 4-char random, örn. `1747300000000-a4f2`.

Gövde:
```json
{
  "from": "<gönderen_ajan>",
  "to": "<hedef_ajan>",
  "body": "<düz metin, markdown OK, embedded envelope yok>",
  "task_id": "<opsiyonel — lifecycle token kullanıyorsa>"
}
```

İzinli hedefler: coordinator, scout, planner, backend-builder, frontend-builder, backend-reviewer, frontend-reviewer, integration-tester, orchestrator. Listede olmayan hedef → `rejected/` + Routing Log panelinde `denied` etiketi.

## 4-State Lifecycle

1. **alındı** — ≤5sn ack.
2. **tamam** — completion + somut sonuç.
3. **belirsiz** — dispatch net değil, sender düzeltsin.
4. **hata** — yapamadı, somut sebep.

Builder/Reviewer ek token'ları: `BUILDING <id>` (opsiyonel), `DONE <id>` (builder→coordinator), `APPROVED <id>` / `CHANGES_NEEDED <id>` (reviewer→coordinator), `TASK_DONE <id>` (backend→orchestrator, otonom fanout).

## Autoflow

Builder DONE → backend bridge.rs paired-reviewer inbox'una `review <id>` düşürür → reviewer APPROVED → backend orchestrator inbox'una `TASK_DONE <id>` düşürür. Manuel re-fanout yasak.

## Çıktı/Doğrulama

- `cargo check --manifest-path src-tauri/Cargo.toml` → 0 error.
- `pnpm --filter app typecheck` → 0 error.
- Routing Log panelinde routed event akışı görünür; `.bridgespace/<session>/processed/` altında dosyalar birikir.
- 9 persona doc'u Write-tool şemasını + lifecycle token'ları + 'Proje konteksi' bloğunu içeriyor.

## İlgili Commit'ler

- a965cad — Persona doc'larına uniform 'Proje konteksi' bloğu (BU REFACTOR'IN PARÇASI).
- 63a248a, de4b739, 17f1f74, d4c656a — eski PTY router'a son fix'ler (bu refactor onları aşıyor).

## Sonraki

WP-W6-02: P1 (useRoutingEvents.ts:6 stale yorum), P3 (design parity audit) ve P4 (commit hijyeni) çıktıları bu WP altında not edilir.
