# Neuron — OTONOM MOD çalışma kuralları (izole worktree)

> Bu dosya, otonom iyileştirme turu sırasında `run.ps1` tarafından worktree'ye kopyalanır ve canlı repo'nun `CLAUDE.md`'sinin yerine geçer. Canlı repo'nun `CLAUDE.md`'si DEĞİŞMEZ.

## Bağlam: burası izole, atılabilir bir worktree

Etkileşimsiz (`claude -p`) çalışıyorsun. Karşında interaktif kullanıcı, çalışan bir dev build YOK. Bu yüzden canlı repo'daki **"Edit'ten önce haber ver / onay bekle"** kuralı **bu bağlamda GEÇERSİZDİR** — soru soracak kimse yok, restart tetiklenecek dev build yok. Kendi kararını ver, uygula, doğrula.

## Yine de mutlak korunan kurallar

1. **`PROJECT_CHARTER.md` Hard Constraints korunur:** mock wire-shape sözleşmesi, OKLCH-only (yeni CSS'te hex/HSL yok), timestamp invariant (`_at`=sn, `_ms`=ms), no Drizzle/JS ORM, dark-first, ADR-0007 ID stratejisi, **`--no-verify` yasak**.
2. **Otorite zinciri:** Charter → WP → design-system-spec → NEURON_TERMINAL_REPORT → `tasks/auto-refactor/PLAYBOOK.md` → AGENTS.md → ADR → kod.
3. **6 doğrulama kapısı zorunlu** (PLAYBOOK §7). Kapıları kır = işi bitirme. Kırarsan düzelt veya geri al.
4. **Kapsam:** tur başına 1–3 ilişkili madde, ≲400 satır diff. Tek tema.
5. **`bindings.ts` elle düzenlenmez** — `pnpm gen:bindings`.
6. **Hot-zone (dirty) dosyalar audit-only** — `.run-context.md`'deki listeye bak; oraya kod değişikliği yazma.
7. **Worktree dışına yazma. commit/push/git apply yapma.** Runner toplar.
8. **Reference-only:** `Neuron Design/`, `neuron-docs/` (varsa) — dokunma.

## Raporlama tonu (AGENTS.md mirası)

Olgusal. "harika!", "mükemmel!" yok. Yapamadığını "yapıldı" deme. Kanıt = dosya:satır / test adı.
