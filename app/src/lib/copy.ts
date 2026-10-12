// User-facing Turkish copy for run status labels and AppError
// variants. Keyed off the wire-format discriminants from
// `bindings.ts` (and the snake_case `kind` strings declared in
// `src-tauri/src/error.rs::AppError::kind`).

const RUN_STATUS_LABELS: Record<string, string> = {
  running: 'Çalışıyor',
  success: 'Başarılı',
  error: 'Başarısız',
};

export function runStatusLabel(status: string): string {
  return RUN_STATUS_LABELS[status] ?? status;
}

const RUN_FILTER_LABELS: Record<string, string> = {
  all: 'Tümü',
  running: 'Çalışıyor',
  success: 'Başarılı',
  error: 'Başarısız',
};

export function runStatusFilterLabel(filter: string): string {
  return RUN_FILTER_LABELS[filter] ?? filter;
}

// Keys here mirror `AppError::kind()` in src-tauri/src/error.rs.
// Anything missing falls back to the generic Turkish copy.
const APP_ERROR_COPY: Record<string, string> = {
  not_found: 'Kayıt bulunamadı.',
  conflict: 'İşlem mevcut bir kayıtla çakıştı.',
  invalid_input: 'Girilen değer geçersiz.',
  db_error: 'Veritabanı hatası — tekrar dene.',
  sidecar: 'Ajan çalışma ortamı çevrimdışı.',
  no_api_key: 'API anahtarı yapılandırılmamış — Ayarlar’dan ekle.',
  mcp_protocol: 'MCP sunucusuyla iletişim hatası.',
  mcp_server_spawn_failed: 'MCP sunucusu başlatılamadı.',
  claude_binary_missing: 'Claude CLI bulunamadı — kurulumu kontrol et.',
  swarm_invoke: 'Ajan çağrısı başarısız oldu.',
  timeout: 'İşlem zaman aşımına uğradı.',
  workspace_busy: 'Bu çalışma alanında zaten devam eden bir iş var.',
  cancelled: 'İptal edildi.',
  internal: 'Beklenmedik bir iç hata oluştu.',
};

export const APP_ERROR_FALLBACK = 'Bilinmeyen hata, tekrar dene.';

export function appErrorCopyByKind(kind: string): string {
  return APP_ERROR_COPY[kind] ?? APP_ERROR_FALLBACK;
}
