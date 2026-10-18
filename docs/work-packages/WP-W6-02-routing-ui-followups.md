# WP-W6-02 — Routing UI takipleri (P1 / P3 / P4)

WP-W6-01'in "Sonraki" bölümünde ayrılan üç takip + bunlarla çakışan
backlog maddeleri (T1-05, T1-06, T2-06). W6 (`swarm_term/**`) PR #1–#3
ile `main`'e girdi; hot-zone kalktığı için ⏸ olan T2-06 uygulanabilir oldu.

## P1 — `useRoutingEvents.ts` stale yorum

- Başlık yorumundaki outcome listesi backend emit noktalarıyla
  (`bridge/{watcher,delivery,synth,stall}.rs`, `session.rs`) tek tek
  karşılaştırıldı: 11 outcome birebir. Tek sapma `lifecycle_fanout`
  açıklamasıydı — bridge artık `review` / `TASK_DONE`'a ek olarak
  `retest` ve `TASK_FAILED` de sentezliyor; yorum güncellendi.
- Frontend `LIFECYCLE_STATE_TO_PHASE` anahtarları `LifecycleState`
  enum'unun `{:?}` çıktısıyla (`Assigned`…`Failed`) eşleşiyor — sapma yok.

## T2-06 — `useRoutingEvents.ts` iki sorumluluğa bölündü

- `hooks/useRoutingEvents.ts` (371L → 174L): yalnız `swarm-term:route`
  module-store + `useRoutingEvents` + `RouteEvent`/`RouteOutcome`/`ALL_OUTCOMES`.
- Yeni `hooks/useAgentLifecycle.ts`: `useActiveEdge`, `useLifecycleEvents`,
  `useAgentLifecycle`, `AgentLifecycle`, `ActiveEdge` (gövdeler birebir taşındı).
- 5 import sitesi yeni yola çevrildi (barrel yok): `TerminalSwarm.tsx`,
  `SwarmHierarchy.tsx`, `swarm-term/{SwarmTermGrid,LifecyclePill,constants}`.
- Yeni `hooks/useAgentLifecycle.test.tsx` (6 test): heuristic fazları,
  reviewer → `review`, ok-dışı / 60 sn dışı olayların yok sayılması,
  backend `swarm-term:lifecycle` olayının heuristic'i ezmesi, active-edge TTL.

## P3 / T1-05 — design parity (OKLCH) denetimi

- `app/src/styles/*.css` hex/rgb/hsl taraması: tek ihlal
  `swarm-term.css` `.swarm-term-pane-body { background: #0a0a0f }` →
  aynı rengin OKLCH karşılığı `oklch(0.147 0.011 285)` (xterm canvas
  arka planıyla dikişsiz kalsın diye birebir dönüşüm).
- Routing Log filtre chip'leri ham `oklch(...)` literal'leri
  kullanıyordu, yanındaki `routing-log-tag-*` ise `--syn-*` token'larını —
  chip'ler de `--syn-success/error/warning/running` token'larına bağlandı
  (tema ile birlikte değişir).
- Bilinçli istisnalar (JS, CSS değil): `useXtermPane.ts` xterm teması
  (canvas renk parser'ı), `useAppearance.ts` accent swatch'ları.

## T1-06 — ölü CSS

- Tüm CSS sınıfları kaynakta arandı; eşleşmeyenlerin hepsi dinamik
  (`chip-outcome-${o}`, `routing-log-tag-${outcome}`,
  `swarm-term-pane-phase--${phase}`, `st-*`, `kind-*`, `status-*`, `toast--*`)
  ya da design-system utility'si (`text-*`). Silinecek ölü sınıf yok.

## P4 — commit hijyeni

- W6 sonrası commit'ler conventional-commit biçiminde, PR başına
  odaklı; bu WP de tek konulu commit olarak girer.

## Doğrulama

- `pnpm typecheck` ✓, `pnpm lint` ✓, `pnpm test --run` 112/112 ✓
  (106 → 112). Backend'e dokunulmadı.
