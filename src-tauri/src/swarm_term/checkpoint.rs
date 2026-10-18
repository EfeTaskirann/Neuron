//! The git safety net: what the agents changed, and the way back.
//!
//! When a session starts, the project's working tree is saved as a git
//! *tree* — tracked files as they are on disk (uncommitted edits
//! included) plus the untracked ones — through a private index file.
//! The user's branch, index and files are never touched, and no commit
//! or ref is made: the only trace in the repository is the objects git
//! writes for any `git add`. Unreferenced, git prunes them after its
//! usual grace period (two weeks).
//!
//! The Changes panel compares that checkpoint with the tree as it is now
//! (the same private index, refreshed), shows each file's diff, and puts
//! a file back the way a checkout writes it — through git's filters, so
//! line endings follow the repository's settings, as with `git stash`.
//! "Keep" moves the checkpoint to now.
//!
//! The snapshot leaves out untracked files in dependency / build folders
//! a project forgot to ignore ([`SKIPPED_DIRS`]) and untracked files over
//! [`MAX_UNTRACKED_BYTES`] — unless the checkpoint holds them already. A
//! project with more than [`MAX_UNTRACKED_FILES`] untracked files is
//! checkpointed without them. Neuron's own bridge folder never counts.
//!
//! What Neuron keeps, per project (`<app_data>/swarm-term/checkpoints/`):
//! the checkpoint's tree id and the private index.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;

use crate::swarm_term::bridge::BRIDGE_DIRNAME;
use crate::swarm_term::pane_config;
use crate::swarm_term::project_guard;

/// Untracked folders a snapshot never takes in (as any path component).
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    ".next",
    ".turbo",
    ".pytest_cache",
    ".mypy_cache",
];

/// Untracked files above this size are left out of a snapshot.
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;

/// More untracked files than this and the checkpoint goes without them
/// (later snapshots add at most this many new ones).
const MAX_UNTRACKED_FILES: usize = 5_000;

/// The panel lists at most this many changed files ...
const MAX_LISTED: usize = 500;

/// ... and shows at most this much of one file's diff.
const MAX_DIFF_BYTES: usize = 200 * 1024;

/// What a folder without git gets instead of a checkpoint.
const NOT_A_REPO: &str = "Bu klasör bir git deposu değil: Neuron ajanların değişikliklerini \
                          gösteremez ve geri alamaz.";

/// One checkpoint operation at a time: they share the private index.
static LOCK: Mutex<()> = Mutex::new(());

/// How a file differs from its checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// A file became a link, or the other way round.
    TypeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SwarmTermChangedFile {
    /// Relative to the project folder, `/`-separated.
    pub path: String,
    pub kind: ChangeKind,
    /// Lines added / removed; `None` for a binary file.
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

/// What changed in a project since its checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SwarmTermChanges {
    /// When the checkpoint was taken (unix seconds); `None`: there is
    /// none for this folder (yet).
    pub checkpoint_at: Option<i64>,
    pub files: Vec<SwarmTermChangedFile>,
    /// More files changed than are listed.
    pub truncated: bool,
    /// Why nothing can be shown, or what the snapshots leave out.
    pub note: Option<String>,
}

/// A project's checkpoint, as Neuron keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    /// The project folder (as `project_guard::display_path` shows it).
    project: String,
    /// The repository's top folder, and the project's place in it
    /// (`""`, or `sub/dir/`).
    top: PathBuf,
    prefix: String,
    /// The checkpoint: a tree id.
    tree: String,
    created_at: i64,
    session_id: String,
    /// Untracked files are part of the snapshots.
    untracked: bool,
    note: Option<String>,
}

/// Save `project` as it is now as its checkpoint (session start). `Ok`
/// carries a note on what the snapshot leaves out; `Err` says why there
/// is no checkpoint (not a git repository, no git).
pub(crate) fn take(app_data: &Path, project: &Path, session_id: &str) -> Result<Option<String>, String> {
    let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (top, prefix) = repo_of(project)?;
    let project_name = name_of(project);
    let (meta, index) = files_of(app_data, &project_name);
    std::fs::create_dir_all(meta.parent().expect("checkpoint dir"))
        .map_err(|e| format!("checkpoint klasörü: {e}"))?;
    // Start from the user's index: its stat data keeps the snapshot cheap.
    let real_index = git(&top, None, &["rev-parse", "--git-path", "index"], None)?;
    let real_index = top.join(String::from_utf8_lossy(&real_index).trim());
    let _ = std::fs::remove_file(&index);
    if real_index.is_file() {
        std::fs::copy(&real_index, &index).map_err(|e| format!("git index: {e}"))?;
    }
    let mut s = Stored {
        project: project_name,
        top,
        prefix,
        tree: String::new(),
        created_at: unix_now(),
        session_id: session_id.to_string(),
        untracked: true,
        note: None,
    };
    update_tracked(&s, &index)?;
    let untracked = new_untracked(&s, &index)?;
    if untracked.len() > MAX_UNTRACKED_FILES {
        s.untracked = false;
        s.note = Some(format!(
            "Projede {} izlenmeyen dosya var: checkpoint yalnız git'in izlediği dosyaları kapsıyor.",
            untracked.len()
        ));
    } else {
        add_paths(&s, &index, &untracked)?;
    }
    s.tree = write_tree(&s, &index)?;
    save(&meta, &s)?;
    Ok(s.note)
}

/// What changed in `project` since its checkpoint. Problems (no
/// checkpoint, no git, a checkpoint git has pruned) come back as the
/// note, never as an error: the panel says them.
pub(crate) fn changes(app_data: &Path, project: &Path) -> SwarmTermChanges {
    let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    match load(app_data, project) {
        Ok(Some((s, index))) => list_changes(&s, &index).unwrap_or_else(|why| SwarmTermChanges {
            checkpoint_at: Some(s.created_at),
            files: Vec::new(),
            truncated: false,
            note: Some(why),
        }),
        Ok(None) => SwarmTermChanges {
            checkpoint_at: None,
            files: Vec::new(),
            truncated: false,
            note: Some("Bu klasör için checkpoint yok: swarm başlatılınca alınır.".into()),
        },
        Err(why) => SwarmTermChanges { checkpoint_at: None, files: Vec::new(), truncated: false, note: Some(why) },
    }
}

/// One changed file's diff against the checkpoint (unified, capped).
pub(crate) fn file_diff(app_data: &Path, project: &Path, path: &str) -> Result<String, String> {
    let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (s, index) = load(app_data, project)?.ok_or_else(no_checkpoint)?;
    let now = refresh(&s, &index)?.0;
    let (files, _) = diff_files(&s, &now)?;
    if !files.iter().any(|f| f.path == path) {
        return Err(format!("`{path}` checkpoint'ten beri değişmemiş."));
    }
    let spec = literal(&format!("{}{path}", s.prefix));
    let out = git(
        &s.top,
        None,
        &["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", &s.tree, &now, "--", &spec],
        None,
    )?;
    Ok(cap_diff(&out))
}

/// Put changed files back as they were at the checkpoint — `paths`, or
/// every changed file: an added file is deleted, the others get their
/// checkpoint content. Only files the change list holds are touched.
/// Returns what is left changed.
pub(crate) fn revert(app_data: &Path, project: &Path, paths: Option<&[String]>) -> Result<SwarmTermChanges, String> {
    let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (s, index) = load(app_data, project)?.ok_or_else(no_checkpoint)?;
    let now = refresh(&s, &index)?.0;
    let (files, _) = diff_files(&s, &now)?;
    let mut failed: Vec<String> = Vec::new();
    for f in files.iter().filter(|f| paths.map_or(true, |p| p.contains(&f.path))) {
        let Some(dest) = inside(project, &f.path) else {
            failed.push(format!("{}: proje dışında", f.path));
            continue;
        };
        let done = match f.kind {
            ChangeKind::Added => remove_added(project, &dest),
            _ => restore(&s, &format!("{}{}", s.prefix, f.path), &dest),
        };
        if let Err(e) = done {
            failed.push(format!("{}: {e}", f.path));
        }
    }
    let mut left = list_changes(&s, &index)?;
    if !failed.is_empty() {
        left.note = Some(format!("Geri alınamayanlar — {}", failed.join("; ")));
    }
    Ok(left)
}

/// Keep what changed: the checkpoint moves to the project as it is now.
pub(crate) fn keep(app_data: &Path, project: &Path) -> Result<SwarmTermChanges, String> {
    let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut s, index) = load(app_data, project)?.ok_or_else(no_checkpoint)?;
    s.tree = refresh(&s, &index)?.0;
    s.created_at = unix_now();
    let (meta, _) = files_of(app_data, &s.project);
    save(&meta, &s)?;
    list_changes(&s, &index)
}

fn no_checkpoint() -> String {
    "Bu klasör için checkpoint yok.".into()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The project folder as the checkpoint records it: resolved, so every
/// spelling of the folder finds the same checkpoint.
fn name_of(project: &Path) -> String {
    project_guard::display_path(&std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf()))
}

/// Where a project's checkpoint lives: its record and private index.
fn files_of(app_data: &Path, project_name: &str) -> (PathBuf, PathBuf) {
    let digest = Sha256::digest(project_name.to_lowercase().as_bytes());
    let key: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let dir = pane_config::swarm_term_data_dir(app_data).join("checkpoints");
    (dir.join(format!("{key}.json")), dir.join(format!("{key}.index")))
}

fn save(meta: &Path, s: &Stored) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(meta, json).map_err(|e| format!("checkpoint kaydı: {e}"))
}

/// The project's checkpoint and its private index, if it has one.
fn load(app_data: &Path, project: &Path) -> Result<Option<(Stored, PathBuf)>, String> {
    let project_name = name_of(project);
    let (meta, index) = files_of(app_data, &project_name);
    let Ok(raw) = std::fs::read(&meta) else {
        return Ok(None);
    };
    let Ok(s) = serde_json::from_slice::<Stored>(&raw) else {
        return Ok(None);
    };
    if s.project != project_name {
        return Ok(None);
    }
    if git(&s.top, None, &["cat-file", "-e", &s.tree], None).is_err() {
        return Err("Checkpoint git deposunda artık yok (depo temizlenmiş olabilir).".into());
    }
    if !index.is_file() {
        // The private index went missing: rebuild it from the checkpoint
        // (without stat data, so the next snapshot reads every file).
        git(&s.top, Some(&index), &["read-tree", &s.tree], None)?;
    }
    Ok(Some((s, index)))
}

/// The repository `project` is in: its top folder and the project's
/// prefix in it.
fn repo_of(project: &Path) -> Result<(PathBuf, String), String> {
    let out = git(project, None, &["rev-parse", "--show-toplevel", "--show-prefix"], None).map_err(|why| {
        if why.contains("not a git repository") {
            NOT_A_REPO.to_string()
        } else {
            why
        }
    })?;
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines();
    let top = lines.next().map(str::trim).filter(|l| !l.is_empty()).ok_or_else(|| NOT_A_REPO.to_string())?;
    let prefix = lines.next().map(str::trim).unwrap_or("").to_string();
    Ok((PathBuf::from(top), prefix))
}

/// A pathspec naming `path` (relative to the repository top) literally.
fn literal(path: &str) -> String {
    format!(":(literal){}", if path.is_empty() { "." } else { path })
}

/// Run git in `dir`, on `index` when given; stdout, or git's complaint.
fn git(dir: &Path, index: Option<&Path>, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        // Settings that would change what is read or written here: no
        // filesystem monitor, no untracked cache in the private index, no
        // LF/CRLF warnings, raw paths.
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.safecrlf=false",
            "-c",
            "core.quotepath=false",
        ])
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    // No console window flashing up from the GUI app.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git bulunamadı: Neuron değişiklikleri git ile izler.".to_string()
        } else {
            format!("git: {e}")
        }
    })?;
    if let Some(input) = stdin {
        // From a thread: git may fill its stdout before it has read all
        // of its stdin.
        let mut pipe = child.stdin.take().expect("piped stdin");
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The private index takes in every change to the files it holds
/// (edits, deletions) under the project.
fn update_tracked(s: &Stored, index: &Path) -> Result<(), String> {
    git(&s.top, Some(index), &["add", "-u", "--", &literal(&s.prefix)], None).map(|_| ())
}

/// Untracked files under the project that the private index does not
/// hold yet and a snapshot takes (see the module docs).
fn new_untracked(s: &Stored, index: &Path) -> Result<Vec<String>, String> {
    let out = git(
        &s.top,
        Some(index),
        &["ls-files", "-z", "--others", "--exclude-standard", "--", &literal(&s.prefix)],
        None,
    )?;
    Ok(out
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| takes_untracked(&s.top, &s.prefix, p))
        .collect())
}

/// Pure-ish: does a snapshot take in the untracked file `path` (relative
/// to the repository top)?
fn takes_untracked(top: &Path, prefix: &str, path: &str) -> bool {
    let in_project = path.strip_prefix(prefix).unwrap_or(path);
    if in_project.split('/').next() == Some(BRIDGE_DIRNAME) {
        return false;
    }
    if path.split('/').any(|c| SKIPPED_DIRS.contains(&c)) {
        return false;
    }
    std::fs::metadata(top.join(path)).is_ok_and(|m| m.is_file() && m.len() <= MAX_UNTRACKED_BYTES)
}

fn add_paths(s: &Stored, index: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut list: Vec<u8> = Vec::new();
    for p in paths {
        list.extend_from_slice(p.as_bytes());
        list.push(0);
    }
    git(&s.top, Some(index), &["update-index", "--add", "-z", "--stdin"], Some(&list)).map(|_| ())
}

fn write_tree(s: &Stored, index: &Path) -> Result<String, String> {
    let out = git(&s.top, Some(index), &["write-tree"], None)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// The project as it is now, as a tree; plus whether new untracked files
/// were left out for their number.
fn refresh(s: &Stored, index: &Path) -> Result<(String, bool), String> {
    update_tracked(s, index)?;
    let mut capped = false;
    if s.untracked {
        let mut new = new_untracked(s, index)?;
        if new.len() > MAX_UNTRACKED_FILES {
            new.truncate(MAX_UNTRACKED_FILES);
            capped = true;
        }
        add_paths(s, index, &new)?;
    }
    Ok((write_tree(s, index)?, capped))
}

fn list_changes(s: &Stored, index: &Path) -> Result<SwarmTermChanges, String> {
    let (now, capped) = refresh(s, index)?;
    let (files, truncated) = diff_files(s, &now)?;
    let note = match (capped, &s.note) {
        (true, _) => Some(format!(
            "Çok sayıda yeni dosya var: ilk {MAX_UNTRACKED_FILES} tanesi izleniyor."
        )),
        (false, note) => note.clone(),
    };
    Ok(SwarmTermChanges { checkpoint_at: Some(s.created_at), files, truncated, note })
}

/// The files that differ between the checkpoint and `now`, relative to
/// the project; and whether the list was cut at [`MAX_LISTED`].
fn diff_files(s: &Stored, now: &str) -> Result<(Vec<SwarmTermChangedFile>, bool), String> {
    let scope = literal(&s.prefix);
    let status = git(&s.top, None, &["diff", "--no-renames", "--name-status", "-z", &s.tree, now, "--", &scope], None)?;
    let numstat = git(&s.top, None, &["diff", "--no-renames", "--numstat", "-z", &s.tree, now, "--", &scope], None)?;
    let mut files = parse_changes(&status, &numstat, &s.prefix);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = files.len() > MAX_LISTED;
    files.truncate(MAX_LISTED);
    Ok((files, truncated))
}

/// Pure: `git diff --name-status -z` + `--numstat -z` (no renames) to the
/// project's changed files, paths made relative to the project (`prefix`
/// stripped); the bridge folder is left out.
fn parse_changes(status: &[u8], numstat: &[u8], prefix: &str) -> Vec<SwarmTermChangedFile> {
    let mut counts: std::collections::HashMap<String, (Option<u32>, Option<u32>)> =
        std::collections::HashMap::new();
    for record in numstat.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        counts.insert(path.to_string(), (added.parse().ok(), deleted.parse().ok()));
    }
    let mut out = Vec::new();
    let mut tokens = status.split(|&b| b == 0).filter(|t| !t.is_empty());
    while let (Some(code), Some(path)) = (tokens.next(), tokens.next()) {
        let path = String::from_utf8_lossy(path).into_owned();
        let kind = match code.first() {
            Some(b'A') => ChangeKind::Added,
            Some(b'D') => ChangeKind::Deleted,
            Some(b'T') => ChangeKind::TypeChanged,
            _ => ChangeKind::Modified,
        };
        let (additions, deletions) = counts.get(&path).copied().unwrap_or((None, None));
        let in_project = path.strip_prefix(prefix).unwrap_or(&path).to_string();
        if in_project.split('/').next() == Some(BRIDGE_DIRNAME) {
            continue;
        }
        out.push(SwarmTermChangedFile { path: in_project, kind, additions, deletions });
    }
    out
}

/// Pure: `path` (relative, `/`-separated) under `project`, or `None` when
/// it would leave it.
fn inside(project: &Path, path: &str) -> Option<PathBuf> {
    let rel = Path::new(path);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| project.join(rel))
}

/// The cut a diff gets for the panel: at a line end, with a marker.
fn cap_diff(out: &[u8]) -> String {
    if out.len() <= MAX_DIFF_BYTES {
        return String::from_utf8_lossy(out).into_owned();
    }
    let cut = out[..MAX_DIFF_BYTES].iter().rposition(|&b| b == b'\n').map_or(MAX_DIFF_BYTES, |i| i + 1);
    format!("{}\n… (diff kısaltıldı)\n", String::from_utf8_lossy(&out[..cut]))
}

/// Delete a file the agents added, and the folders it leaves empty.
fn remove_added(project: &Path, dest: &Path) -> Result<(), String> {
    match std::fs::remove_file(dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let mut dir = dest.parent();
    while let Some(d) = dir {
        if d == project || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// Write the checkpoint's version of `full` (relative to the repository
/// top) to `dest`, through git's checkout filters (line endings, ...).
fn restore(s: &Stored, full: &str, dest: &Path) -> Result<(), String> {
    let object = format!("{}:{full}", s.tree);
    let kind = git(&s.top, None, &["cat-file", "-t", &object], None)?;
    if String::from_utf8_lossy(&kind).trim() != "blob" {
        return Err("checkpoint'te dosya değil".into());
    }
    let content = git(&s.top, None, &["cat-file", "--filters", &object], None)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, content).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(git(dir, None, args, None).unwrap_or_else(|e| panic!("git {args:?}: {e}"))).unwrap()
    }

    /// A repository tracking a CRLF file (autocrlf) and a plain one, with
    /// an untracked file next to them. `None` when git is not installed.
    fn repo() -> Option<(tempfile::TempDir, PathBuf)> {
        git(Path::new("."), None, &["--version"], None).ok()?;
        let base = tempfile::tempdir().expect("tempdir");
        let r = base.path().join("repo");
        std::fs::create_dir_all(r.join("src")).unwrap();
        git_ok(&r, &["init", "-q"]);
        git_ok(&r, &["config", "core.autocrlf", "true"]);
        std::fs::write(r.join("src/lib.rs"), "fn a() {}\r\nfn b() {}\r\n").unwrap();
        std::fs::write(r.join("README.md"), "readme\n").unwrap();
        git_ok(&r, &["add", "-A"]);
        std::fs::write(r.join("notes.txt"), "mine, untracked\n").unwrap();
        Some((base, r))
    }

    fn kinds(c: &SwarmTermChanges) -> Vec<(String, ChangeKind)> {
        c.files.iter().map(|f| (f.path.clone(), f.kind)).collect()
    }

    #[test]
    fn the_checkpoint_sees_every_change_and_touches_nothing() {
        let Some((base, r)) = repo() else { return };
        let app = base.path().join("app");
        // Uncommitted work of the user's, before the session.
        std::fs::write(r.join("README.md"), "readme, edited\n").unwrap();
        let status_before = git_ok(&r, &["status", "--porcelain"]);
        let refs_before = git_ok(&r, &["for-each-ref"]);
        assert_eq!(take(&app, &r, "s1"), Ok(None));
        // Nothing moved: index, files, refs.
        assert_eq!(git_ok(&r, &["status", "--porcelain"]), status_before);
        assert_eq!(git_ok(&r, &["for-each-ref"]), refs_before);
        assert!(changes(&app, &r).files.is_empty(), "no change yet");

        // The agents edit, add, delete — also the untracked file.
        std::fs::write(r.join("src/lib.rs"), "fn a() {}\r\nfn b() { todo!() }\r\nfn c() {}\r\n").unwrap();
        std::fs::write(r.join("src/new.rs"), "pub fn n() {}\n").unwrap();
        std::fs::remove_file(r.join("README.md")).unwrap();
        std::fs::write(r.join("notes.txt"), "overwritten\n").unwrap();
        let c = changes(&app, &r);
        assert_eq!(
            kinds(&c),
            [
                ("README.md".into(), ChangeKind::Deleted),
                ("notes.txt".into(), ChangeKind::Modified),
                ("src/lib.rs".into(), ChangeKind::Modified),
                ("src/new.rs".into(), ChangeKind::Added),
            ]
        );
        let lib = c.files.iter().find(|f| f.path == "src/lib.rs").unwrap();
        assert_eq!((lib.additions, lib.deletions), (Some(2), Some(1)));
        assert!(c.checkpoint_at.is_some() && c.note.is_none());
        let d = file_diff(&app, &r, "src/lib.rs").unwrap();
        assert!(d.contains("+fn c() {}") && d.contains("-fn b() {}"), "{d}");
        // Only a changed file has a diff to show.
        assert!(file_diff(&app, &r, "Cargo.toml").is_err());
    }

    #[test]
    fn reverting_puts_files_back_as_git_would_check_them_out() {
        let Some((base, r)) = repo() else { return };
        let app = base.path().join("app");
        take(&app, &r, "s1").unwrap();
        let lib_before = std::fs::read(r.join("src/lib.rs")).unwrap();
        std::fs::write(r.join("src/lib.rs"), "broken\n").unwrap();
        std::fs::create_dir_all(r.join("src/gen/deep")).unwrap();
        std::fs::write(r.join("src/gen/deep/x.rs"), "x\n").unwrap();
        std::fs::remove_file(r.join("notes.txt")).unwrap();
        std::fs::write(r.join("README.md"), "kept edit\n").unwrap();

        // One file first ...
        let left = revert(&app, &r, Some(&["src/lib.rs".to_string()])).unwrap();
        assert_eq!(std::fs::read(r.join("src/lib.rs")).unwrap(), lib_before, "CRLF line endings come back");
        assert!(!left.files.iter().any(|f| f.path == "src/lib.rs"));
        // ... then the rest, except what the user keeps.
        let left = revert(&app, &r, Some(&["src/gen/deep/x.rs".to_string(), "notes.txt".to_string()])).unwrap();
        assert!(!r.join("src/gen").exists(), "an added file goes, with the folders it leaves empty");
        // Line endings as a checkout writes them (`git stash` alike): this
        // LF file comes back with CRLF under autocrlf — the same file to git.
        assert_eq!(std::fs::read_to_string(r.join("notes.txt")).unwrap(), "mine, untracked\r\n");
        assert_eq!(kinds(&left), [("README.md".into(), ChangeKind::Modified)]);
        // A path the change list does not hold is never touched.
        let left = revert(&app, &r, Some(&["src/main.rs".to_string()])).unwrap();
        assert_eq!(left.files.len(), 1);
        assert!(!r.join("src/main.rs").exists());

        // Keep: the checkpoint moves to now; nothing is left to revert.
        let kept = keep(&app, &r).unwrap();
        assert!(kept.files.is_empty());
        assert!(revert(&app, &r, None).unwrap().files.is_empty());
        assert_eq!(std::fs::read_to_string(r.join("README.md")).unwrap(), "kept edit\n");
    }

    #[test]
    fn build_output_bridge_files_and_big_files_are_not_snapshotted() {
        let Some((base, r)) = repo() else { return };
        let app = base.path().join("app");
        take(&app, &r, "s1").unwrap();
        for dir in ["target/debug", "node_modules/x", ".bridgespace/swarm-term-1/outbox"] {
            std::fs::create_dir_all(r.join(dir)).unwrap();
            std::fs::write(r.join(dir).join("f.txt"), "x").unwrap();
        }
        std::fs::write(r.join(".bridgespace/.gitignore"), "*\n!.gitignore\n").unwrap();
        std::fs::write(r.join("big.bin"), vec![b'x'; (MAX_UNTRACKED_BYTES + 1) as usize]).unwrap();
        std::fs::write(r.join("small.txt"), "s\n").unwrap();
        assert_eq!(kinds(&changes(&app, &r)), [("small.txt".into(), ChangeKind::Added)]);
    }

    #[test]
    fn a_project_inside_a_repository_sees_only_its_own_files() {
        let Some((base, r)) = repo() else { return };
        let app = base.path().join("app");
        let project = r.join("src");
        take(&app, &project, "s1").unwrap();
        std::fs::write(r.join("src/lib.rs"), "changed\n").unwrap();
        std::fs::write(r.join("README.md"), "outside the project\n").unwrap();
        assert_eq!(kinds(&changes(&app, &project)), [("lib.rs".into(), ChangeKind::Modified)]);
        revert(&app, &project, None).unwrap();
        assert!(std::fs::read_to_string(r.join("src/lib.rs")).unwrap().contains("fn a()"));
        assert_eq!(std::fs::read_to_string(r.join("README.md")).unwrap(), "outside the project\n");
    }

    #[test]
    fn a_folder_without_git_has_no_checkpoint_and_says_so() {
        if git(Path::new("."), None, &["--version"], None).is_err() {
            return;
        }
        let base = tempfile::tempdir().expect("tempdir");
        let plain = base.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let app = base.path().join("app");
        assert_eq!(take(&app, &plain, "s1"), Err(NOT_A_REPO.to_string()));
        let c = changes(&app, &plain);
        assert!(c.checkpoint_at.is_none() && c.files.is_empty());
        assert!(c.note.unwrap().contains("checkpoint yok"));
        assert!(revert(&app, &plain, None).is_err());
    }

    #[test]
    fn the_change_list_is_read_from_gits_nul_separated_output() {
        let status = b"M\0app/src/a.ts\0A\0app/new file.md\0D\0app/old.rs\0A\0app/.bridgespace/.gitignore\0";
        let numstat = b"3\t1\tapp/src/a.ts\0-\t-\tapp/new file.md\0";
        assert_eq!(
            parse_changes(status, numstat, "app/"),
            [
                SwarmTermChangedFile { path: "src/a.ts".into(), kind: ChangeKind::Modified, additions: Some(3), deletions: Some(1) },
                SwarmTermChangedFile { path: "new file.md".into(), kind: ChangeKind::Added, additions: None, deletions: None },
                SwarmTermChangedFile { path: "old.rs".into(), kind: ChangeKind::Deleted, additions: None, deletions: None },
            ]
        );
        assert!(inside(Path::new("C:/p"), "src/a.rs").is_some());
        for bad in ["../x", "/etc/x", "src/../../x", "C:/x"] {
            assert!(inside(Path::new("C:/p"), bad).is_none(), "{bad}");
        }
        let big = format!("{}\nend", "line\n".repeat(MAX_DIFF_BYTES / 5 + 10));
        let cut = cap_diff(big.as_bytes());
        assert!(cut.len() < big.len() && cut.ends_with("(diff kısaltıldı)\n"));
    }
}
