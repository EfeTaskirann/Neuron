//! Refuses project folders the swarm must not run in.
//!
//! Every pane gets `Read(./**)` on the project and builders get
//! `Edit(./**)` (see `permissions`), so the project folder is the
//! agents' reach. A drive root, the user's home (or anything above it),
//! a per-user app-data root (browser profiles, other apps' tokens,
//! Neuron's own pane homes with their credential copies), a credential
//! store (`~/.ssh`, `~/.claude`, …) or an OS folder would put far more
//! than a project in that reach — in either direction: the project may
//! neither sit inside such a folder nor contain one.
//!
//! The temp folder is scratch space: a project strictly inside it is
//! allowed even though it sits under an app-data root on Windows
//! (`%LOCALAPPDATA%\Temp`).

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::swarm_term::home_isolation::real_user_home;

/// Folders under the user's home that hold credentials.
const CREDENTIAL_DIRS: &[&str] = &[".ssh", ".claude", ".aws", ".azure", ".gnupg", ".kube", ".docker"];

/// OS folders on Unix-likes (Windows reads them from the environment).
#[cfg(not(windows))]
const UNIX_SYSTEM_DIRS: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/proc", "/sbin", "/sys", "/usr",
    "/System", "/Library", "/private/etc",
];

/// What a guarded folder holds — picks the refusal wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardedKind {
    AppData,
    Credentials,
    System,
}

/// Why a project folder is refused.
#[derive(Debug, PartialEq, Eq)]
enum Rejection {
    /// A drive / filesystem root.
    Root,
    /// The user's home folder or a folder that contains it.
    HomeOrAbove,
    /// Inside, or containing, a guarded folder.
    Guarded { dir: PathBuf, kind: GuardedKind },
}

/// The canonical folders the check compares against.
#[derive(Debug, Default)]
struct Guards {
    /// The project may live under these, but may not be one or hold one.
    homes: Vec<PathBuf>,
    /// The project may neither be inside nor contain these.
    guarded: Vec<(PathBuf, GuardedKind)>,
    /// Scratch space: paths strictly inside it skip `guarded`.
    temp: Option<PathBuf>,
}

/// Refuses `project` (an existing directory) when the swarm must not run
/// there. The error message says why, in the user's words.
pub(crate) fn check<R: Runtime>(app: &AppHandle<R>, project: &Path) -> Result<(), AppError> {
    let canonical = std::fs::canonicalize(project).map_err(|e| {
        AppError::InvalidInput(format!(
            "`{}` çözümlenemedi ({e}); swarm yalnızca erişilebilir bir proje klasöründe başlar.",
            project.display()
        ))
    })?;
    match rejection(&canonical, &collect(app)) {
        None => Ok(()),
        Some(r) => Err(AppError::InvalidInput(message(&r, project))),
    }
}

/// Pure: why `project` (canonical) is refused, if it is.
fn rejection(project: &Path, guards: &Guards) -> Option<Rejection> {
    if project.parent().is_none() {
        return Some(Rejection::Root);
    }
    if guards.homes.iter().any(|home| home.starts_with(project)) {
        return Some(Rejection::HomeOrAbove);
    }
    if guards
        .temp
        .as_deref()
        .is_some_and(|temp| project != temp && project.starts_with(temp))
    {
        return None;
    }
    guards
        .guarded
        .iter()
        .find(|(dir, _)| project.starts_with(dir) || dir.starts_with(project))
        .map(|(dir, kind)| Rejection::Guarded {
            dir: dir.clone(),
            kind: *kind,
        })
}

fn message(rejection: &Rejection, project: &Path) -> String {
    let p = project.display();
    let reach = "Swarm ajanları proje klasörünün tamamını okuyabilir, builder'lar da yazabilir";
    match rejection {
        Rejection::Root => {
            format!("`{p}` bir sürücü kökü. {reach}: kökü değil, bir proje klasörünü seç.")
        }
        Rejection::HomeOrAbove => format!(
            "`{p}` ev klasörün ya da onu içeren bir klasör. {reach}; .ssh ve .claude gibi \
             kimlik bilgileri de bu kapsama girerdi: bir proje klasörü seç."
        ),
        Rejection::Guarded { dir, kind } => {
            let what = match kind {
                GuardedKind::AppData => "uygulama veri klasörü",
                GuardedKind::Credentials => "kimlik bilgisi klasörü",
                GuardedKind::System => "sistem klasörü",
            };
            format!(
                "`{p}`, bir {what} olan `{}` ile iç içe. {reach}: bir proje klasörü seç.",
                display_path(dir)
            )
        }
    }
}

/// Canonical Windows paths carry the verbatim prefix; show them the way
/// the user writes them (`\\?\C:\x` → `C:\x`, `\\?\UNC\srv\s` → `\\srv\s`).
fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    }
}

/// The guarded folders of this machine and user. A folder that does not
/// exist (or does not resolve) cannot contain the project and is skipped.
fn collect<R: Runtime>(app: &AppHandle<R>) -> Guards {
    let paths = app.path();
    let raw_homes: Vec<PathBuf> = [paths.home_dir().ok(), real_user_home().ok()]
        .into_iter()
        .flatten()
        .collect();

    let mut guards = Guards {
        temp: std::fs::canonicalize(std::env::temp_dir()).ok(),
        ..Guards::default()
    };
    for home in &raw_homes {
        push_unique(&mut guards.homes, home);
    }

    let app_data = [
        paths.data_dir(),
        paths.local_data_dir(),
        paths.config_dir(),
        paths.app_data_dir(),
        paths.app_local_data_dir(),
    ];
    for dir in app_data.into_iter().flatten() {
        push_guarded(&mut guards.guarded, &dir, GuardedKind::AppData);
    }
    for home in &raw_homes {
        for name in CREDENTIAL_DIRS {
            push_guarded(&mut guards.guarded, &home.join(name), GuardedKind::Credentials);
        }
        if cfg!(target_os = "macos") {
            // Keychains and every app's data.
            push_guarded(&mut guards.guarded, &home.join("Library"), GuardedKind::Credentials);
        }
    }
    for dir in system_dirs() {
        push_guarded(&mut guards.guarded, &dir, GuardedKind::System);
    }
    guards
}

#[cfg(windows)]
fn system_dirs() -> Vec<PathBuf> {
    [
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ]
    .iter()
    .filter_map(std::env::var_os)
    .filter(|v| !v.is_empty())
    .map(PathBuf::from)
    .collect()
}

#[cfg(not(windows))]
fn system_dirs() -> Vec<PathBuf> {
    UNIX_SYSTEM_DIRS.iter().map(PathBuf::from).collect()
}

fn push_unique(list: &mut Vec<PathBuf>, dir: &Path) {
    if let Ok(canonical) = std::fs::canonicalize(dir) {
        if !list.contains(&canonical) {
            list.push(canonical);
        }
    }
}

fn push_guarded(list: &mut Vec<(PathBuf, GuardedKind)>, dir: &Path, kind: GuardedKind) {
    if let Ok(canonical) = std::fs::canonicalize(dir) {
        if !list.iter().any(|(d, _)| *d == canonical) {
            list.push((canonical, kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic machine: `<root>/Users/me` home with an app-data root,
    /// a credential folder, an OS folder, and temp under the app data.
    fn machine() -> (PathBuf, Guards) {
        let root = std::env::temp_dir()
            .ancestors()
            .last()
            .expect("a root")
            .to_path_buf();
        let home = root.join("Users").join("me");
        let local = home.join("AppData").join("Local");
        let guards = Guards {
            homes: vec![home.clone()],
            guarded: vec![
                (local.clone(), GuardedKind::AppData),
                (home.join(".ssh"), GuardedKind::Credentials),
                (root.join("Windows"), GuardedKind::System),
            ],
            temp: Some(local.join("Temp")),
        };
        (root, guards)
    }

    fn guarded(dir: PathBuf, kind: GuardedKind) -> Option<Rejection> {
        Some(Rejection::Guarded { dir, kind })
    }

    #[test]
    fn roots_and_the_home_or_above_are_refused() {
        let (root, g) = machine();
        assert_eq!(rejection(&root, &g), Some(Rejection::Root));
        let home = root.join("Users").join("me");
        assert_eq!(rejection(&home, &g), Some(Rejection::HomeOrAbove));
        assert_eq!(rejection(&root.join("Users"), &g), Some(Rejection::HomeOrAbove));
    }

    #[test]
    fn ordinary_projects_under_the_home_are_allowed() {
        let (root, g) = machine();
        let home = root.join("Users").join("me");
        assert_eq!(rejection(&home.join("code").join("app"), &g), None);
        assert_eq!(rejection(&home.join("OneDrive").join("Desktop").join("Neuron"), &g), None);
        // Component-wise: `WindowsApps` is not inside `Windows`.
        assert_eq!(rejection(&root.join("WindowsApps").join("x"), &g), None);
    }

    #[test]
    fn guarded_folders_are_refused_inside_and_above() {
        let (root, g) = machine();
        let home = root.join("Users").join("me");
        let local = home.join("AppData").join("Local");
        let ssh = home.join(".ssh");
        assert_eq!(rejection(&ssh, &g), guarded(ssh.clone(), GuardedKind::Credentials));
        assert_eq!(
            rejection(&ssh.join("keys"), &g),
            guarded(ssh.clone(), GuardedKind::Credentials)
        );
        assert_eq!(
            rejection(&local.join("Google").join("Chrome"), &g),
            guarded(local.clone(), GuardedKind::AppData)
        );
        // Containing a guarded folder counts too.
        assert_eq!(
            rejection(&home.join("AppData"), &g),
            guarded(local.clone(), GuardedKind::AppData)
        );
        assert_eq!(
            rejection(&root.join("Windows").join("System32"), &g),
            guarded(root.join("Windows"), GuardedKind::System)
        );
    }

    #[test]
    fn scratch_projects_in_temp_are_allowed_but_not_temp_itself() {
        let (root, g) = machine();
        let local = root.join("Users").join("me").join("AppData").join("Local");
        let temp = local.join("Temp");
        assert_eq!(rejection(&temp.join("scratch").join("proj"), &g), None);
        assert_eq!(rejection(&temp, &g), guarded(local, GuardedKind::AppData));
    }

    #[test]
    fn messages_name_the_folder_without_the_verbatim_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\C:\Users\me")), r"C:\Users\me");
        assert_eq!(display_path(Path::new(r"\\?\UNC\srv\share")), r"\\srv\share");
        assert_eq!(display_path(Path::new("/home/me")), "/home/me");
        let msg = message(
            &Rejection::Guarded {
                dir: PathBuf::from(r"\\?\C:\Users\me\AppData\Local"),
                kind: GuardedKind::AppData,
            },
            Path::new(r"C:\Users\me\AppData"),
        );
        assert!(msg.contains(r"`C:\Users\me\AppData\Local`"), "{msg}");
        assert!(msg.contains("uygulama veri klasörü"), "{msg}");
        assert!(message(&Rejection::Root, Path::new("C:\\")).contains("sürücü kökü"));
    }

    #[test]
    fn this_machine_refuses_home_root_and_app_data_but_not_projects() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let handle = app.handle();
        // The checkout and a scratch folder in temp are ordinary projects.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("repo");
        assert!(check(handle, repo).is_ok(), "{:?}", check(handle, repo));
        let scratch = tempfile::tempdir().expect("tempdir");
        assert!(check(handle, scratch.path()).is_ok(), "{:?}", check(handle, scratch.path()));

        let home = handle.path().home_dir().expect("home");
        let refused = |p: &Path, needle: &str| match check(handle, p) {
            Err(AppError::InvalidInput(m)) => m.contains(needle),
            other => panic!("{} not refused: {other:?}", p.display()),
        };
        assert!(refused(&home, "ev klasörün"));
        assert!(refused(home.ancestors().last().expect("root"), "sürücü kökü"));
        if let Ok(data) = handle.path().data_dir() {
            if data.is_dir() {
                assert!(refused(&data, "uygulama veri klasörü"));
            }
        }
    }
}
