# Neuron — Claude çalışma kuralları

## Edit'ten önce haber ver

Bu repo'daki kaynak dosyalara (`app/`, `src-tauri/`, config dosyaları, `src-tauri/src/swarm/agents/**/*.md` persona dosyaları, vb.) `Edit` / `Write` / `NotebookEdit` uygulamadan önce:

1. Tek cümleyle hangi dosyayı, ne için değiştireceğini söyle.
2. Kullanıcının onayını bekle — veya edit gerektirmeyen bir alternatif öner (sadece inceleme/açıklama, ya da kullanıcının elden yapacağı küçük bir değişiklik).
3. Sessiz toplu edit yapma. Birden fazla dosya değişecekse hepsini önce listele.

**Neden:** Kullanıcı dev build'i aktif olarak çalıştırıyor. Edit'ler restart tetikliyor ve devam eden oturum/iş yarım kalıyor.

**Uygulanmaz:**
- Salt-okur işlemler (`Read`, `Grep`, `Glob`, `git status`/`git diff` gibi inceleme komutları) — bunlar restart tetiklemiyor, ön bildirim gerekmez.
- Kullanıcının çalıştırmadığı saf not/dökümantasyon dosyaları (kullanıcı zaten istemişse).

**"devam" hakkında:** Kullanıcı "devam" dediğinde bu, **daha önce duyurulmuş** plana yeşil ışıktır — duyurulmamış edit'ler için açık çek değildir.
