use crate::error::Result;
use crate::install::{create_symlinks, remove_symlinks, InstallState, InstalledPackage};
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, warn};

pub(crate) struct KegBackup {
    name: String,
    record: InstalledPackage,
    cellar: PathBuf,
    keg: PathBuf,
    backup: PathBuf,
}

impl KegBackup {
    pub(crate) async fn take(name: &str) -> Option<Self> {
        let record = InstallState::new()
            .ok()?
            .load()
            .await
            .ok()?
            .get(name)?
            .clone();
        let cellar = record.install_mode.cellar_path().ok()?;
        let keg = cellar.join(name).join(&record.version);
        if !keg.is_dir() {
            return None;
        }
        let backup = cellar
            .parent()?
            .join(".wax-upgrade")
            .join(format!("{name}-{}", record.version));
        if let Err(e) = Self::move_aside(&record, &cellar, &keg, &backup).await {
            debug!("not backing up {}: {}", name, e);
            return None;
        }
        Some(Self {
            name: name.to_string(),
            record,
            cellar,
            keg,
            backup,
        })
    }

    async fn move_aside(
        record: &InstalledPackage,
        cellar: &std::path::Path,
        keg: &std::path::Path,
        backup: &std::path::Path,
    ) -> Result<()> {
        remove_symlinks(
            &record.name,
            &record.version,
            cellar,
            false,
            record.install_mode,
        )
        .await?;
        if fs::symlink_metadata(backup).await.is_ok() {
            fs::remove_dir_all(backup).await?;
        }
        fs::create_dir_all(backup.parent().unwrap()).await?;
        if let Err(e) = fs::rename(keg, backup).await {
            create_symlinks(
                &record.name,
                &record.version,
                cellar,
                false,
                record.install_mode,
            )
            .await?;
            return Err(e.into());
        }
        Ok(())
    }
    pub(crate) async fn restore(self) -> Result<()> {
        if fs::symlink_metadata(&self.keg).await.is_ok() {
            fs::remove_dir_all(&self.keg).await?;
        }
        fs::create_dir_all(self.keg.parent().unwrap()).await?;
        fs::rename(&self.backup, &self.keg).await?;
        create_symlinks(
            &self.name,
            &self.record.version,
            &self.cellar,
            false,
            self.record.install_mode,
        )
        .await?;
        InstallState::new()?.add(self.record).await?;
        Ok(())
    }

    pub(crate) async fn discard(self) {
        if let Err(e) = fs::remove_dir_all(&self.backup).await {
            warn!(
                "could not remove upgrade backup {}: {}",
                self.backup.display(),
                e
            );
        }
        if let Some(parent) = self.backup.parent() {
            let _ = fs::remove_dir(parent).await;
        }
    }
    pub(crate) async fn settle<T>(backup: Option<Self>, result: &Result<T>) -> bool {
        let Some(backup) = backup else {
            return false;
        };
        if result.is_ok() {
            backup.discard().await;
            return false;
        }
        let name = backup.name.clone();
        match backup.restore().await {
            Ok(()) => true,
            Err(e) => {
                warn!("could not restore {} after failed upgrade: {}", name, e);
                false
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::install::{InstallMode, HOME_MUTEX};

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn failed_upgrade_restores_keg_links_and_record() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        let home_path = dunce::canonicalize(home.path()).unwrap();
        std::env::set_var("HOME", &home_path);
        let prefix = home_path.join(".local/wax");
        let keg = prefix.join("Cellar/tool/1.0");
        fs::create_dir_all(keg.join("bin")).await.unwrap();
        fs::write(keg.join("bin/tool"), b"x").await.unwrap();
        create_symlinks(
            "tool",
            "1.0",
            &prefix.join("Cellar"),
            false,
            InstallMode::User,
        )
        .await
        .unwrap();
        let state = InstallState::new().unwrap();
        state
            .add(InstalledPackage {
                name: "tool".into(),
                version: "1.0".into(),
                platform: "test".into(),
                install_date: 0,
                install_mode: InstallMode::User,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: None,
                pinned: false,
            })
            .await
            .unwrap();

        let backup = KegBackup::take("tool").await.unwrap();
        assert!(!keg.exists());
        assert!(fs::symlink_metadata(prefix.join("bin/tool")).await.is_err());
        state.remove("tool").await.unwrap();
        fs::remove_dir_all(prefix.join("Cellar/tool"))
            .await
            .unwrap();

        let failed: Result<()> = Err(crate::error::WaxError::InstallError("boom".into()));
        assert!(KegBackup::settle(Some(backup), &failed).await);
        assert!(keg.join("bin/tool").exists());
        assert!(prefix.join("bin/tool").exists());
        assert_eq!(state.load().await.unwrap()["tool"].version, "1.0");

        let backup = KegBackup::take("tool").await.unwrap();
        assert!(!KegBackup::settle(Some(backup), &Ok(())).await);
        assert!(!prefix.join(".wax-upgrade").exists());
    }
}
