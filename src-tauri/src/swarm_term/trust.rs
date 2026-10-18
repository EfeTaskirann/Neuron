//! The user's agreement, per project folder, to run the swarm there.
//!
//! The agents read the project, builders change it, and running its
//! tests or build executes its code with the user's privileges — so the
//! swarm only starts in a folder the user has trusted, once, in Neuron's
//! own dialog that says this. Claude Code asks the same question for
//! every folder it has not seen ("Do you trust the files in this
//! folder?"); after the user's yes in Neuron, the session answers that
//! screen in the panes. Kept in Neuron's settings table, never in the
//! user's Claude configuration.

use std::path::Path;

use crate::commands::settings;
use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm_term::project_guard::display_path;

/// Settings key: a JSON array of trusted folders (canonical paths).
const TRUSTED_PROJECTS_KEY: &str = "swarm_term.trusted_projects";

/// The folder as stored: canonical (symlinks, `.`, case resolved), and
/// without Windows' `\\?\` prefix.
fn folder_key(project: &Path) -> Result<String, AppError> {
    let canon = std::fs::canonicalize(project).map_err(|e| {
        AppError::InvalidInput(format!("project folder {}: {e}", project.display()))
    })?;
    Ok(display_path(&canon))
}

async fn trusted_folders(pool: &DbPool) -> Result<Vec<String>, AppError> {
    Ok(settings::read(pool, TRUSTED_PROJECTS_KEY)
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

/// Has the user trusted this folder?
pub(crate) async fn is_trusted(pool: &DbPool, project: &Path) -> Result<bool, AppError> {
    let key = folder_key(project)?;
    Ok(trusted_folders(pool).await?.contains(&key))
}

/// Record the user's trust in this folder (idempotent).
pub(crate) async fn trust(pool: &DbPool, project: &Path) -> Result<(), AppError> {
    let key = folder_key(project)?;
    let mut folders = trusted_folders(pool).await?;
    if folders.contains(&key) {
        return Ok(());
    }
    folders.push(key);
    let json = serde_json::to_string(&folders)
        .map_err(|e| AppError::Internal(format!("trusted folders: {e}")))?;
    settings::write(pool, TRUSTED_PROJECTS_KEY, &json).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn trust_is_per_folder_and_survives_other_spellings() {
        let (pool, _db) = crate::test_support::fresh_pool().await;
        let base = tempfile::tempdir().expect("tempdir");
        let project = base.path().join("proj");
        let other = base.path().join("other");
        std::fs::create_dir_all(project.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        assert!(!is_trusted(&pool, &project).await.unwrap());
        trust(&pool, &project).await.unwrap();
        trust(&pool, &project).await.unwrap(); // idempotent
        assert!(is_trusted(&pool, &project).await.unwrap());
        // The same folder through `sub/..` is the same folder ...
        assert!(is_trusted(&pool, &project.join("sub").join("..")).await.unwrap());
        // ... a sibling or a subfolder is not.
        assert!(!is_trusted(&pool, &other).await.unwrap());
        assert!(!is_trusted(&pool, &project.join("sub")).await.unwrap());
        assert_eq!(trusted_folders(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_missing_folder_is_an_input_error_and_garbage_reads_as_untrusted() {
        let (pool, _db) = crate::test_support::fresh_pool().await;
        let base = tempfile::tempdir().expect("tempdir");
        let err = is_trusted(&pool, &base.path().join("nope")).await.unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
        settings::write(&pool, TRUSTED_PROJECTS_KEY, "not json").await.unwrap();
        assert!(!is_trusted(&pool, base.path()).await.unwrap());
        trust(&pool, base.path()).await.unwrap();
        assert!(is_trusted(&pool, base.path()).await.unwrap());
    }
}
