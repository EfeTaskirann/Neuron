//! Persona-injection payload construction for the terminal swarm.
//!
//! [`build_persona_payload`] wraps an agent's persona text plus the
//! bridge-protocol footer in bracketed-paste escapes so a claude REPL
//! treats the whole thing as one pasted+submitted message. The footer
//! is the only routing instruction an agent ever sees: the Write-tool
//! JSON envelope schema, the hierarchy-gated destination list, the
//! 4-state lifecycle vocabulary, and the builder/reviewer tokens.
//!
//! Extracted from `session.rs` so the frequently prompt-tuned footer
//! template lives apart from pane spawn/teardown logic.

use std::path::Path;

use crate::swarm_term::hierarchy::allowed_for;

/// Build the persona-injection payload for one agent.
///
/// The body wraps the persona text + the bridge-protocol footer in
/// terminal **bracketed-paste** escape sequences (`\x1b[200~ … \x1b[201~`)
/// so claude's REPL treats it as a single pasted message instead of
/// many separate Enter-submitted lines, then submits with a final `\r`.
///
/// The bridge protocol section is the only routing instruction the
/// persona ever sees. It tells claude:
///   * how to send a message — use the `Write` tool with a specific
///     absolute path under the per-session bridge root,
///   * the exact JSON schema of the envelope,
///   * which destinations are reachable (the hierarchy gate),
///   * the 4-state lifecycle vocabulary (alındı / tamam / belirsiz /
///     hata) that backs the autonomy contract,
///   * how received messages arrive (bracketed-paste signed with
///     `— from @<sender> [routed by Neuron]`),
///   * the first-response convention (`@<agent_id> hazır.`).
///
/// `bridge_root` is the absolute path to the per-session
/// `.bridgespace/<session>/` directory. The footer renders absolute
/// paths so claude doesn't need to interpolate environment variables
/// (which it does inconsistently when generating `Write` tool calls).
pub(crate) fn build_persona_payload(
    agent_id: &str,
    body: &str,
    bridge_root: &Path,
) -> String {
    let allowed: Vec<&str> = allowed_for(agent_id).to_vec();
    let bridge_root_str = path_as_forward_slashes(bridge_root);

    // Allowed-destinations block. Each line gives the literal Write
    // path template — claude can copy the prefix and substitute the
    // ULID/timestamp filename.
    let dest_lines: Vec<String> = if allowed.is_empty() {
        vec!["(yok — bu ajan kimseye mesaj gönderemiyor)".to_string()]
    } else {
        allowed
            .iter()
            .map(|dst| {
                format!(
                    "  - `@{dst}` → `Write` path: `{bridge_root_str}/inbox/{dst}/<ULID>.json`"
                )
            })
            .collect()
    };
    let dest_block = dest_lines.join("\n");

    // Pick an example destination for the worked example. Falls back
    // to "orchestrator" when the agent has no allowed destinations
    // (shouldn't happen in the canonical graph, but defensive).
    let example_dst: &str = allowed.first().copied().unwrap_or("orchestrator");

    let footer = format!(
        "\n\n## Mesajlaşma protokolü — KRİTİK\n\n\
         Sen bu swarm'ın `{agent_id}` ajanısın. Diğer ajanlara mesaj göndermek için\n\
         `Write` tool'unu kullan — atomik JSON dosyası yaz. PTY'a `>> @x:` gibi bir\n\
         satır yazmak ARTIK YOK; o eski API'ydi, kaldırıldı.\n\n\
         ### Dosya yolu şeması\n\n\
         Her mesaj için yeni bir dosya yarat:\n\n\
         `{bridge_root_str}/inbox/<HEDEF_AJAN>/<MSG_ID>.json`\n\n\
         `<HEDEF_AJAN>` — aşağıdaki izinli hedeflerden biri.\n\
         `<MSG_ID>` — her mesaj için benzersiz; basit format: unix-epoch-ms + 4-char\n\
         random suffix, örn. `1747300000000-a4f2.json`. Aynı dosya adını iki kez\n\
         yazma; üzerine yazılır ve eski mesaj kaybolur.\n\n\
         ### JSON şeması (tam olarak bu alanları kullan)\n\n\
         ```json\n\
         {{\n  \"from\": \"{agent_id}\",\n  \"to\": \"<HEDEF_AJAN>\",\n  \"body\": \"<mesaj gövdesi — Türkçe ya da İngilizce>\",\n  \"task_id\": \"<opsiyonel; lifecycle token kullanıyorsan koy>\"\n}}\n\
         ```\n\n\
         `from` alanı SENİN id'in (`{agent_id}`) olmalı. Body bütün metin — newline'lar\n\
         JSON içinde `\\n` olarak escape edilir; uzun mesajlarda korkma, sınır yok.\n\n\
         ### İzinli hedeflerin\n\n{dest_block}\n\n\
         Listede olmayan hedefe yazarsan backend `rejected/` klasörüne taşır ve\n\
         Routing Log panelinde `denied` etiketiyle gösterir; mesaj iletilmez.\n\n\
         ### Çalışma akışı (her dispatch için)\n\n\
         1. Mesajını planla. Hangi hedef ajan? Hangi spesifik talimat?\n\
         2. `Write` tool'unu çağır: path = yukarıdaki şablon, content = JSON.\n\
         3. Tool çağrısı tamamlandıktan sonra başka bir şey yazma — backend dosyayı\n\
            ≤250ms içinde alır, hedef pane'e bracketed-paste eder, ve dosyayı\n\
            `processed/<HEDEF>/` altına taşır.\n\n\
         ### Worked example\n\n\
         Diyelim ki `{example_dst}` ajanına \"foo.rs:42 incelendi, refactor önerisi:\n\
         X\" göndermek istiyorsun. Şunu yap:\n\n\
         Tool: `Write`\n\
         Path: `{bridge_root_str}/inbox/{example_dst}/1747300000000-a4f2.json`\n\
         Content:\n\
         ```json\n\
         {{\n  \"from\": \"{agent_id}\",\n  \"to\": \"{example_dst}\",\n  \"body\": \"foo.rs:42 incelendi — fonksiyon 80 satır, 3 farklı sorumluluk taşıyor. Refactor önerisi: A) extract guard clauses, B) split into validate_input + apply_change, C) inline single-use helper.\"\n}}\n\
         ```\n\n\
         ### Sana gelen mesajlar\n\n\
         Senin pane'ine bracketed-paste olarak gelir. Alt kısımda `— from @<gönderen>\n\
         [routed by Neuron]` imzası bulunur — bu imzaya bakarak kime cevap vereceğini\n\
         (genelde gönderene) bil. Mesajı verbatim alıntılama; paraphrase et.\n\n\
         ### 4-state lifecycle contract (zorunlu)\n\n\
         Sana bir dispatch gelirse SUS KALMA; 4 durumdan birini gönderene yolla:\n\n\
         1. **alındı** (≤5sn içinde): `body: \"alındı — <bir cümlelik anlayışın>\"`.\n\
            Acknowledgement. Sender bilgi sahibi olur, polling yapmaz.\n\
         2. **tamam** (iş bitince): `body: \"tamam — <somut sonuç, dosya yolları>\"`.\n\
            Completion. Sender bir sonraki faza geçer.\n\
         3. **belirsiz** (dispatch net değilse): `body: \"belirsiz — <spesifik sorun:\n\
            hangi dosya / hangi tür değişiklik / kabul kriteri ne>\"` ve DUR.\n\
            Tahmin yapma; tahminle çalışırsan reviewer reject eder.\n\
         4. **hata** (yapamadıysan): `body: \"hata — <somut sebep>\"` ve dur.\n\n\
         **Gönderen tarafsan, ALDIĞIN state'e göre:**\n\
         - `alındı —` → SUS. Specialist çalışıyor; ikinci dispatch atma, polling yapma.\n\
         - `tamam —` → completion'ı kabul et, bir sonraki faza geç.\n\
         - `belirsiz —` → aynı vague task'i tekrar gönderme; spesifik sorunu çöz ve\n\
           yeni, somut bir dispatch yaz.\n\
         - `hata —` → retry mı, alternative specialist mi, escalate mi karar ver.\n\
           Aynı dispatch'i tekrar yollama (3 kez denersen reviewer reject sayar).\n\n\
         ### Builder/Reviewer lifecycle token'ları (yalnızca rolüne uygunsa kullan)\n\n\
         Bunlar `body` içindeki özel prefix'ler — `task_id` alanını da doldur. Backend\n\
         bunları görür, lifecycle state machine'i ilerletir, gerekiyorsa otomatik\n\
         fanout yapar (örn. builder DONE → reviewer'a otomatik `review <id>` dispatch).\n\n\
         - **Builder ise** iş bitince: `body: \"DONE <task_id>\"` (coordinator'e gönder).\n\
         - **Reviewer ise** onay verince: `body: \"APPROVED <task_id>\"` (coordinator'e).\n\
         - **Reviewer ise** değişiklik istiyorsa: `body: \"CHANGES_NEEDED <task_id>\"`\n\
           ve ne istediğini açıkla.\n\
         - **Builder ise** çalışmaya başladığını duyurmak istiyorsan (opsiyonel):\n\
           `body: \"BUILDING <task_id>\"`.\n\n\
         ### İlk yanıt (bu persona mesajından sonra)\n\n\
         Bu persona mesajını aldıktan sonra **yalnızca** şunu yaz ve dur:\n\
         `@{agent_id} hazır.` Başka tek karakter yazma. Bir sonraki user/route\n\
         mesajını bekle.",
        agent_id = agent_id,
        bridge_root_str = bridge_root_str,
        dest_block = dest_block,
        example_dst = example_dst,
    );

    format!("\x1b[200~{body}{footer}\x1b[201~\r")
}

fn path_as_forward_slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dummy_bridge_root() -> PathBuf {
        // Tests only use the path string — no IO. A platform-realistic
        // absolute path covers the forward-slash conversion behaviour.
        if cfg!(target_os = "windows") {
            PathBuf::from("C:\\Users\\efe\\proj\\.bridgespace\\swarm-term-01HZQ")
        } else {
            PathBuf::from("/home/efe/proj/.bridgespace/swarm-term-01HZQ")
        }
    }

    #[test]
    fn payload_is_bracketed_paste_wrapped() {
        let p = build_persona_payload("scout", "Hello body", &dummy_bridge_root());
        assert!(p.starts_with('\x1b'));
        assert!(p.contains("\x1b[200~"));
        assert!(p.contains("\x1b[201~"));
        assert!(p.ends_with('\r'));
    }

    #[test]
    fn payload_carries_persona_body_verbatim() {
        let body = "# Scout\n\nFind things.\n";
        let p = build_persona_payload("scout", body, &dummy_bridge_root());
        assert!(p.contains(body));
    }

    #[test]
    fn payload_includes_bridge_protocol_and_allowed_destinations() {
        let p = build_persona_payload("scout", "x", &dummy_bridge_root());
        assert!(p.contains("## Mesajlaşma protokolü"));
        assert!(p.contains("`Write` tool"));
        assert!(p.contains("inbox/<HEDEF_AJAN>"));
        // scout's allowed destinations.
        assert!(p.contains("@coordinator"));
        assert!(p.contains("@orchestrator"));
        assert!(p.contains("@planner"));
    }

    #[test]
    fn payload_embeds_absolute_bridge_path_with_forward_slashes() {
        // The persona must show the actual bridge path so claude can
        // use it in Write tool calls. Forward slashes on all platforms
        // — claude accepts them on Windows and they read cleanly.
        let p = build_persona_payload("scout", "x", &dummy_bridge_root());
        let expected_fragment = if cfg!(target_os = "windows") {
            "C:/Users/efe/proj/.bridgespace/swarm-term-01HZQ"
        } else {
            "/home/efe/proj/.bridgespace/swarm-term-01HZQ"
        };
        assert!(
            p.contains(expected_fragment),
            "payload missing absolute bridge path; got body:\n{p}"
        );
    }

    #[test]
    fn payload_does_not_teach_pty_marker_grammar() {
        // Belt-and-suspenders: any column-0 `>> @<agent>:` in the
        // injected text would be useless legacy doc. Pin that the new
        // payload doesn't contain that pattern.
        let p = build_persona_payload("orchestrator", "x", &dummy_bridge_root());
        for line in p.split('\n') {
            assert!(
                !line.starts_with(">> @"),
                "persona footer has bare `>> @` at column 0 — relic of old API: {line}"
            );
        }
    }

    #[test]
    fn payload_includes_lifecycle_protocol() {
        let p = build_persona_payload("scout", "x", &dummy_bridge_root());
        assert!(p.contains("alındı"));
        assert!(p.contains("tamam"));
        assert!(p.contains("belirsiz"));
        assert!(p.contains("hata"));
    }

    #[test]
    fn payload_includes_lifecycle_tokens() {
        // Builders + reviewers must see DONE / APPROVED /
        // CHANGES_NEEDED in the protocol description so they know to
        // emit them — the bridge's lifecycle parser keys on these
        // prefixes for the autofanout step.
        let p = build_persona_payload(
            "backend-builder",
            "x",
            &dummy_bridge_root(),
        );
        assert!(p.contains("DONE <task_id>"));
        assert!(p.contains("APPROVED <task_id>"));
        assert!(p.contains("CHANGES_NEEDED <task_id>"));
    }

    #[test]
    fn payload_for_isolated_agent_says_yok() {
        // The hardcoded graph has no isolated agents, but the helper
        // must still be defensible against an unknown id.
        let allowed = allowed_for("nobody");
        assert!(allowed.is_empty());
        let p = build_persona_payload("nobody", "x", &dummy_bridge_root());
        assert!(p.contains("(yok"));
    }
}
