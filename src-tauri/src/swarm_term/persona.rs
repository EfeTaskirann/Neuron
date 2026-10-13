//! Persona construction for the terminal swarm.
//!
//! [`build_persona_prompt`] renders an agent's persona text plus the
//! bridge-protocol footer. The session writes it to a file and hands it
//! to claude via `--append-system-prompt-file`, so the protocol lives in
//! the SYSTEM prompt: it survives auto-compaction and carries system-
//! prompt weight (as a pasted user message it was observed being
//! overridden by claude's default "ask before acting" habit).
//! [`build_kickoff_message`] is the short pasted first turn that makes
//! the agent answer `@<id> hazır.`, which the session uses as its
//! readiness signal.
//!
//! The footer is the only routing instruction an agent ever sees: the
//! Write-tool JSON envelope schema, the hierarchy-gated destination
//! list, the 4-state lifecycle vocabulary, and the builder/reviewer
//! tokens.
//!
//! Extracted from `session.rs` so the frequently prompt-tuned footer
//! template lives apart from pane spawn/teardown logic.

use std::path::Path;

use crate::swarm_term::hierarchy::allowed_for;
use crate::swarm_term::lifecycle::MAX_REWORK_ROUNDS;

/// First line of the kickoff turn; the footer tells the agent to answer
/// a message starting with it with exactly `@<id> hazır.`.
pub(crate) const KICKOFF_PREFIX: &str = "[Neuron] Oturum başladı.";

/// The pasted first turn of every swarm-term pane (bracketed paste, no
/// submitting CR — the caller uses `paste_then_submit`). Deliberately
/// does NOT contain the readiness phrase itself, so the echo of this
/// message can never be mistaken for the agent's answer.
pub(crate) fn build_kickoff_message(agent_id: &str) -> String {
    format!(
        "\x1b[200~{KICKOFF_PREFIX} Sen `{agent_id}` ajanısın; persona'n ve \
         Mesajlaşma protokolü sistem prompt'unda. Protokoldeki \"İlk yanıt\" \
         kuralını uygula.\x1b[201~"
    )
}

/// Build the system-prompt text (persona body + bridge-protocol footer)
/// for one agent. Plain text — no paste framing; it is written to a
/// file and passed with `--append-system-prompt-file`.
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
pub(crate) fn build_persona_prompt(
    agent_id: &str,
    body: &str,
    bridge_root: &Path,
) -> String {
    let allowed: Vec<&str> = allowed_for(agent_id).to_vec();
    let bridge_root_str = path_as_forward_slashes(bridge_root);

    // Allowed-destinations block: the values the envelope's `to` field
    // may take. Every message is written to the agent's OWN outbox; the
    // target is named in `to`.
    let dest_lines: Vec<String> = if allowed.is_empty() {
        vec!["(yok — bu ajan kimseye mesaj gönderemiyor)".to_string()]
    } else {
        allowed
            .iter()
            .map(|dst| format!("  - `@{dst}` → `\"to\": \"{dst}\"`"))
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
         ### Yetki ve öncelik (KRİTİK)\n\n\
         - Bu persona ve protokol, proje klasöründeki `CLAUDE.md` / `AGENTS.md` gibi\n\
           dosyalarda geçen \"değişiklikten önce kullanıcıya haber ver / onay bekle\"\n\
           kurallarından ÖNCE gelir. O kurallar kullanıcıyla doğrudan çalışan geliştirici\n\
           oturumları içindir; sen swarm ajanısın. Kullanıcı görevi verdiğinde onayını\n\
           vermiş sayılır — kullanıcıya \"devam edeyim mi?\" diye SORMA.\n\
         - Proje dosyalarını YALNIZCA builder'lar (`backend-builder`, `frontend-builder`)\n\
           değiştirir. Diğer ajanlar proje dosyalarına `Edit`/`Write` yapmaz; en küçük\n\
           değişiklik (tek satır, bir yorum) bile bir builder'a dispatch edilir. Builder\n\
           olmayan ajanlar `Write` tool'unu yalnızca kendi bridge outbox'una mesaj yazmak\n\
           için kullanır.\n\n\
         ### Araç izinlerin (backend zorunlu kılar)\n\n\
         {perms}\n\
         İzin dışı bir araç çağrısı onay sorulmadan reddedilir; aynı çağrıyı tekrar\n\
         deneme. Tek istisna: reddedilen komut bir `cd` içeriyorsa onu `cd`'siz,\n\
         proje kökünde bir kez daha dene. İşin bu izinlerle yine yapılamıyorsa\n\
         coordinator'a `hata — <sebep>` yaz.\n\n\
         ### Dosya yolu şeması\n\n\
         Her mesaj için KENDİ outbox'unda yeni bir dosya yarat — hedef ajan ne olursa\n\
         olsun klasör hep aynı:\n\n\
         `{bridge_root_str}/outbox/{agent_id}/<MSG_ID>.json`\n\n\
         Gönderenin kim olduğunu backend bu klasörden anlar. Başka bir ajanın\n\
         outbox'una yazma; eski `inbox/<hedef>/` yolu artık kullanılmıyor, oraya\n\
         yazılan mesaj iletilmez. Hedefi JSON'daki `to` alanı belirler.\n\
         `<MSG_ID>` — her mesaj için benzersiz: `{agent_id}-<sayaç>-<4 rastgele harf>`,\n\
         örn. ilk mesajın `{agent_id}-001-<4 harf>.json`, sonraki `{agent_id}-002-…`.\n\
         Sayaç her mesajda bir artar, 4 harfi her seferinde YENİ seç; örnekteki\n\
         harfleri kopyalama. Aynı dosya adını iki kez yazma — üzerine yazılır ve\n\
         eski mesaj kaybolur. Sıralamayı backend dosyanın yazılma zamanından yapar.\n\n\
         ### JSON şeması (tam olarak bu alanları kullan)\n\n\
         ```json\n\
         {{\n  \"from\": \"{agent_id}\",\n  \"to\": \"<HEDEF_AJAN>\",\n  \"body\": \"<mesaj gövdesi — Türkçe ya da İngilizce>\",\n  \"task_id\": \"<opsiyonel; lifecycle token kullanıyorsan koy>\"\n}}\n\
         ```\n\n\
         `to` zorunlu: aşağıdaki izinli hedeflerden biri. `from` opsiyonel; yazarsan\n\
         SENİN id'in (`{agent_id}`) olmalı, başka bir ad yazılan mesaj reddedilir.\n\
         Body bütün metin — newline'lar JSON içinde `\\n` olarak escape edilir.\n\
         Dosya en fazla 256 KB olabilir; daha büyüğü okunmadan reddedilir.\n\n\
         ### İzinli hedeflerin\n\n{dest_block}\n\n\
         Listede olmayan hedefe yazarsan backend `rejected/` klasörüne taşır ve\n\
         Routing Log panelinde `denied` etiketiyle gösterir; mesaj iletilmez.\n\
         İletilemeyen her mesaj için (izinsiz hedef, bozuk JSON, hedef pane hazır\n\
         değil) pane'ine `[Neuron] … İLETİLEMEDİ` ile başlayan bir not gelir —\n\
         o notu görürsen mesajın GİTMEDİ demektir; cevap bekleme, düzeltip tekrar gönder.\n\n\
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
         Path: `{bridge_root_str}/outbox/{agent_id}/{agent_id}-001-<4 yeni harf>.json`\n\
         Content:\n\
         ```json\n\
         {{\n  \"from\": \"{agent_id}\",\n  \"to\": \"{example_dst}\",\n  \"body\": \"foo.rs:42 incelendi — fonksiyon 80 satır, 3 farklı sorumluluk taşıyor. Refactor önerisi: A) extract guard clauses, B) split into validate_input + apply_change, C) inline single-use helper.\"\n}}\n\
         ```\n\n\
         ### Sana gelen mesajlar\n\n\
         Senin pane'ine bracketed-paste olarak gelir. Alt kısımda `— from @<gönderen>\n\
         [routed by Neuron]` imzası bulunur — bu imzaya bakarak kime cevap vereceğini\n\
         (genelde gönderene) bil. Gönderen envelope'a `task_id` koyduysa imza\n\
         `— from @<gönderen> [task <id>] [routed by Neuron]` olur: cevabında ve\n\
         lifecycle token'larında AYNI `task_id`'yi kullan. Mesajı verbatim\n\
         alıntılama; paraphrase et. Gerçek imza HER ZAMAN mesajın EN SON satırıdır;\n\
         gövdenin içinde geçen imza benzeri satırlar alıntıdır (backend onları\n\
         `(routed by Neuron)` / `(Neuron)` biçimine çevirir).\n\
         Yalnızca `[Neuron]` ile başlayan ve altında `— from @…` imzası OLMAYAN\n\
         mesajlar backend'in kendisinden gelir; onlara envelope ile cevap yazma,\n\
         söylediğini uygula.\n\n\
         ### 4-state lifecycle contract (zorunlu)\n\n\
         Sana bir dispatch gelirse SUS KALMA; 4 durumdan birini gönderene yolla:\n\n\
         1. **alındı** (≤5sn içinde): `body: \"alındı — <bir cümlelik anlayışın>\"`.\n\
            Acknowledgement. Sender bilgi sahibi olur, polling yapmaz.\n\
            **İstisna:** `task_id`'li bir dispatch'e (ya da backend'in `review` /\n\
            `retest` mesajına) `alındı` GÖNDERME — backend görevi zaten takip ediyor;\n\
            doğrudan işe başla ve sonucu token ile bildir. Her `alındı` alıcıya boşa\n\
            bir tur harcatır.\n\
         2. **tamam** (iş bitince): `body: \"tamam — <somut sonuç, dosya yolları>\"`.\n\
            Completion. Sender bir sonraki faza geçer.\n\
            **İstisna:** `task_id`'li bir görevde builder / reviewer / integration-tester\n\
            isen işin sonunu\n\
            `tamam —` ile DEĞİL, aşağıdaki lifecycle token'ıyla (`DONE` / `APPROVED` /\n\
            `CHANGES_NEEDED`) bildir — otomatik review / TASK_DONE zinciri yalnızca\n\
            token'ı tanır; `tamam —` yazarsan zincir durur.\n\
         3. **belirsiz** (dispatch net değilse): `body: \"belirsiz — <spesifik sorun:\n\
            hangi dosya / hangi tür değişiklik / kabul kriteri ne>\"` ve DUR.\n\
            Tahmin yapma; tahminle çalışırsan reviewer reject eder.\n\
         4. **hata** (yapamadıysan): `body: \"hata — <somut sebep>\"` ve dur.\n\
            `task_id`'li bir görevse envelope'un `task_id` alanını doldurup\n\
            coordinator'a yolla — backend görevi kapatır ve orchestrator'a\n\
            `TASK_FAILED <task_id>` bildirir. Netleştirilebilecek bir sorunsa `hata`\n\
            değil `belirsiz` yaz; `hata` \"bu görevden vazgeçtim\" demektir.\n\n\
         **Gönderen tarafsan, ALDIĞIN state'e göre:**\n\
         - `alındı —` → SUS. Specialist çalışıyor; ikinci dispatch atma, polling yapma.\n\
         - `tamam —` → completion'ı kabul et, bir sonraki faza geç.\n\
         - `belirsiz —` → aynı vague task'i tekrar gönderme; spesifik sorunu çöz ve\n\
           yeni, somut bir dispatch yaz.\n\
         - `hata —` → aynı dispatch'i aynen tekrar yollama. `task_id`'li görevlerde\n\
           backend görevi zaten kapattı (`TASK_FAILED`); yeniden denemek gerekiyorsa\n\
           YENİ bir task_id ile, sorunu çözen yeni bir dispatch yaz.\n\n\
         ### Lifecycle token'ları (yalnızca rolüne uygunsa kullan)\n\n\
         Bunlar `body` içindeki özel prefix'ler — `task_id` alanını da doldur ve\n\
         HER ZAMAN coordinator'a gönder (başka bir ajana yazarsan backend mesajı\n\
         coordinator'a yönlendirir). Backend bunları görür, lifecycle state machine'i\n\
         ilerletir, gerekiyorsa otomatik fanout yapar (örn. builder DONE → reviewer'a\n\
         otomatik `review <id>` dispatch).\n\n\
         Token body'nin EN BAŞINDA, BÜYÜK harfle, markdown süsü (`**`, `:`) olmadan\n\
         yazılır; ardından bir boşluk ve task_id gelir.\n\n\
         - **Builder ise** iş bitince: `body: \"DONE <task_id> — <ne değişti, hangi dosyalar>\"`.\n\
           `—` sonrasındaki not reviewer'a (ya da yeniden test için tester'a) otomatik\n\
           iletilir; neye bakılacağı oradan öğrenilir, o yüzden dosya yollarını yaz.\n\
         - **Builder ise** çalışmaya başladığını duyurmak istiyorsan (opsiyonel):\n\
           `body: \"BUILDING <task_id>\"`.\n\
         - **Reviewer / integration-tester ise** onay (test PASS) verince:\n\
           `body: \"APPROVED <task_id>\"`.\n\
         - **Reviewer / integration-tester ise** değişiklik gerekiyorsa (test FAIL):\n\
           `body: \"CHANGES_NEEDED <task_id> — <ne değişmeli: dosya, sebep, hangi builder>\"`.\n\
           Düzeltmeyi coordinator dispatch eder; builder'a ayrıca yazma.\n\n\
         Bir görev en fazla {max_rework} düzeltme turu alır: {max_rework_plus}. `CHANGES_NEEDED`'de\n\
         backend görevi kapatır ve orchestrator'a `TASK_FAILED <task_id>` bildirir.\n\
         Kapanmış (`TASK_DONE` / `TASK_FAILED`) bir task_id tekrar kullanılamaz.\n\n\
         ### İlk yanıt\n\n\
         Oturumun ilk mesajı `{kickoff}` ile başlar. O mesaja **yalnızca** şunu yaz\n\
         ve dur: `@{agent_id} hazır.` Başka tek karakter yazma, tool çağırma. Backend\n\
         bu cevabı görünce pane'ini mesaj almaya hazır sayar. Sonra bir sonraki\n\
         user/route mesajını bekle.",
        agent_id = agent_id,
        bridge_root_str = bridge_root_str,
        dest_block = dest_block,
        example_dst = example_dst,
        kickoff = KICKOFF_PREFIX,
        perms = crate::swarm_term::permissions::describe_for_persona(agent_id),
        max_rework = MAX_REWORK_ROUNDS,
        max_rework_plus = MAX_REWORK_ROUNDS + 1,
    );

    format!("{body}{footer}")
}

/// True when `text` (an assistant turn) is `agent_id`'s readiness
/// answer. Tolerant of surrounding whitespace / markdown and a missing
/// trailing dot; requires both the `@<id>` handle and `hazır`.
pub(crate) fn is_ready_answer(agent_id: &str, text: &str) -> bool {
    let t = text.trim().trim_matches(|c| c == '*' || c == '`').to_lowercase();
    t.contains(&format!("@{agent_id}")) && t.contains("hazır")
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
    fn prompt_is_plain_text_without_paste_framing() {
        // System-prompt file content: any terminal escape would end up
        // verbatim in claude's system prompt.
        let p = build_persona_prompt("scout", "Hello body", &dummy_bridge_root());
        assert!(p.starts_with("Hello body"));
        assert!(!p.contains('\x1b'));
        assert!(!p.contains('\r'));
    }

    #[test]
    fn kickoff_is_a_paste_that_does_not_contain_the_ready_phrase() {
        let k = build_kickoff_message("scout");
        assert!(k.starts_with("\x1b[200~"));
        assert!(k.ends_with("\x1b[201~"));
        assert!(k.contains(KICKOFF_PREFIX));
        // The echo of the kickoff must never satisfy the readiness check.
        assert!(!is_ready_answer("scout", &k));
    }

    #[test]
    fn prompt_tells_the_agent_how_to_answer_the_kickoff() {
        let p = build_persona_prompt("scout", "x", &dummy_bridge_root());
        assert!(p.contains(KICKOFF_PREFIX));
        assert!(p.contains("`@scout hazır.`"));
    }

    #[test]
    fn prompt_overrides_project_approval_rules_and_reserves_edits_for_builders() {
        // Live runs: the orchestrator followed the repo CLAUDE.md
        // ("announce, wait for approval") — analysed the task itself and
        // asked the user "devam edeyim mi?" instead of dispatching.
        let p = build_persona_prompt("orchestrator", "x", &dummy_bridge_root());
        assert!(p.contains("### Yetki ve öncelik"));
        assert!(p.contains("CLAUDE.md"));
        assert!(p.contains("\"devam edeyim mi?\" diye SORMA"));
        assert!(p.contains("YALNIZCA builder'lar"));
    }

    #[test]
    fn ready_answer_detection() {
        assert!(is_ready_answer("scout", "@scout hazır."));
        assert!(is_ready_answer("scout", "  **@scout hazır**  "));
        assert!(!is_ready_answer("scout", "@planner hazır."));
        assert!(!is_ready_answer("scout", "@scout başlıyorum"));
    }

    #[test]
    fn prompt_message_id_rule_is_not_a_copyable_literal() {
        // A fixed example id (`1747300000000-a4f2`) was copied verbatim
        // by agents in live runs; the rule must be per-agent + counter.
        let p = build_persona_prompt("scout", "x", &dummy_bridge_root());
        assert!(!p.contains("1747300000000"));
        assert!(p.contains("scout-001-"));
    }

    #[test]
    fn payload_carries_persona_body_verbatim() {
        let body = "# Scout\n\nFind things.\n";
        let p = build_persona_prompt("scout", body, &dummy_bridge_root());
        assert!(p.contains(body));
    }

    #[test]
    fn payload_includes_bridge_protocol_and_allowed_destinations() {
        let p = build_persona_prompt("scout", "x", &dummy_bridge_root());
        assert!(p.contains("## Mesajlaşma protokolü"));
        assert!(p.contains("`Write` tool"));
        assert!(p.contains("outbox/scout/<MSG_ID>.json"));
        assert!(p.contains("\"to\": \"coordinator\""));
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
        let p = build_persona_prompt("scout", "x", &dummy_bridge_root());
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
        let p = build_persona_prompt("orchestrator", "x", &dummy_bridge_root());
        for line in p.split('\n') {
            assert!(
                !line.starts_with(">> @"),
                "persona footer has bare `>> @` at column 0 — relic of old API: {line}"
            );
        }
    }

    #[test]
    fn payload_includes_lifecycle_protocol() {
        let p = build_persona_prompt("scout", "x", &dummy_bridge_root());
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
        let p = build_persona_prompt(
            "backend-builder",
            "x",
            &dummy_bridge_root(),
        );
        assert!(p.contains("DONE <task_id>"));
        assert!(p.contains("APPROVED <task_id>"));
        assert!(p.contains("CHANGES_NEEDED <task_id>"));
    }

    #[test]
    fn footer_has_no_accidentally_indented_lines() {
        // A7: one missing `\` line continuation kept the next source
        // line's 9-space indent; 4+ leading spaces turn a markdown line
        // into a code block (the message-id example rendered as code).
        for &agent in crate::swarm_term::hierarchy::AGENT_IDS {
            let p = build_persona_prompt(agent, "x", &dummy_bridge_root());
            for line in p.lines() {
                let indent = line.len() - line.trim_start_matches(' ').len();
                assert!(indent < 4, "{agent}: over-indented footer line: {line:?}");
            }
        }
    }

    #[test]
    fn footer_states_one_retry_limit_and_the_failure_signal() {
        // B2: the retry limit has one source (MAX_REWORK_ROUNDS) and the
        // old "3 kez denersen reviewer reject sayar" wording is gone.
        let p = build_persona_prompt("coordinator", "x", &dummy_bridge_root());
        assert!(p.contains(&format!("en fazla {MAX_REWORK_ROUNDS} düzeltme turu")));
        assert!(p.contains("TASK_FAILED <task_id>"));
        assert!(!p.contains("3 kez denersen"));
        // Tester verdicts use the same tokens and go to the coordinator.
        let t = build_persona_prompt("integration-tester", "x", &dummy_bridge_root());
        assert!(t.contains("Reviewer / integration-tester ise"));
        assert!(t.contains("HER ZAMAN coordinator'a gönder"));
    }

    #[test]
    fn term_personas_agree_with_the_backend_lifecycle() {
        // B2/B9: persona prose quotes the backend's retry limit, no stale
        // limits remain, and no persona carries tool frontmatter that
        // swarm-term would ignore (permissions come from `permissions`).
        let reg = crate::swarm::profile::ProfileRegistry::load_term(None).expect("load_term");
        let limit = format!("en fazla {MAX_REWORK_ROUNDS} düzeltme turu");
        for id in ["coordinator", "backend-reviewer", "frontend-reviewer"] {
            let body = &reg.get(id).expect(id).body;
            assert!(body.contains(&limit), "{id} must quote `{limit}`");
        }
        for &id in crate::swarm_term::hierarchy::AGENT_IDS {
            let body = &reg.get(id).expect(id).body;
            for stale in ["3 kez", "3 başarısız", "tamam — düzelttim"] {
                assert!(!body.contains(stale), "{id} still says `{stale}`");
            }
        }
        // Live run: the tester approved a smoke with a failing test it
        // called "pre-existing" — any failing test is a FAIL.
        let tester = &reg.get("integration-tester").expect("tester").body;
        assert!(tester.contains("Tek bir test bile başarısızsa"));
        let raw = include_str!("../swarm/agents/term/coordinator.md");
        assert!(!raw.contains("allowed_tools:"), "term personas carry no tool frontmatter");
    }

    #[test]
    fn payload_for_isolated_agent_says_yok() {
        // The hardcoded graph has no isolated agents, but the helper
        // must still be defensible against an unknown id.
        let allowed = allowed_for("nobody");
        assert!(allowed.is_empty());
        let p = build_persona_prompt("nobody", "x", &dummy_bridge_root());
        assert!(p.contains("(yok"));
    }
}
