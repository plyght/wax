use super::*;
use tempfile::TempDir;

fn formula(version: &str) -> String {
    format!(
        r#"class Example < Formula
  desc "Fixture formula"
  homepage "https://example.invalid/"
  url "https://example.invalid/example-{version}.tar.gz"
  version "{version}"
  sha256 "{}"
  def install
    bin.install "example"
  end
end
"#,
        "a".repeat(64)
    )
}

fn cask(version: &str) -> String {
    format!(
        r#"cask "example" do
  version "{version}"
  sha256 "{}"
  url "https://example.invalid/example.zip"
  name "Example"
  desc "Fixture cask"
  homepage "https://example.invalid/"
  app "Example.app"
end
"#,
        "a".repeat(64)
    )
}

fn fixture() -> (TempDir, Tap, TapIndexStore) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("source");
    std::fs::create_dir_all(root.join("Formula")).unwrap();
    std::fs::create_dir_all(root.join("Casks")).unwrap();
    let tap = Tap {
        full_name: "fixture/tools".into(),
        kind: TapKind::LocalDir { path: root.clone() },
        path: root,
    };
    let store = TapIndexStore::new(&tmp.path().join("cache"), &tap.full_name);
    (tmp, tap, store)
}

#[tokio::test]
async fn unchanged_snapshot_is_not_rewritten_and_paths_are_restored() {
    let (_tmp, tap, store) = fixture();
    let formula_path = tap.path.join("Formula/example.rb");
    let cask_path = tap.path.join("Casks/example.rb");
    fs::write(&formula_path, formula("1.0")).await.unwrap();
    fs::write(&cask_path, cask("1.0")).await.unwrap();
    let _lock = store.lock().await.unwrap();
    let first = store.refresh(&tap).await.unwrap();
    let bytes = fs::read(&store.index_path).await.unwrap();
    let modified = fs::metadata(&store.index_path)
        .await
        .unwrap()
        .modified()
        .unwrap();
    let second = store.refresh(&tap).await.unwrap();
    assert_eq!(fs::read(&store.index_path).await.unwrap(), bytes);
    assert_eq!(
        fs::metadata(&store.index_path)
            .await
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert_eq!(second.signature().unwrap(), first.signature().unwrap());
    assert_eq!(second.formulae()[0].rb_path.as_ref(), Some(&formula_path));
    assert_eq!(second.casks()[0].rb_path.as_ref(), Some(&cask_path));
    assert!(second.formulae()[0].bottle.is_none());
}

#[tokio::test]
async fn adds_modifies_deletes_and_renames_both_package_kinds() {
    let (_tmp, tap, store) = fixture();
    let _lock = store.lock().await.unwrap();
    fs::write(tap.path.join("Formula/keep.rb"), formula("1.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Formula/remove.rb"), formula("1.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Casks/old.rb"), cask("1.0"))
        .await
        .unwrap();
    store.refresh(&tap).await.unwrap();
    fs::write(tap.path.join("Formula/keep.rb"), formula("2.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Formula/add.rb"), formula("3.0"))
        .await
        .unwrap();
    fs::remove_file(tap.path.join("Formula/remove.rb"))
        .await
        .unwrap();
    fs::rename(tap.path.join("Casks/old.rb"), tap.path.join("Casks/new.rb"))
        .await
        .unwrap();
    fs::write(tap.path.join("Casks/new.rb"), cask("2.0"))
        .await
        .unwrap();
    let index = store.refresh(&tap).await.unwrap();
    let formulae = index.formulae();
    assert_eq!(
        formulae.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["add", "keep"]
    );
    assert_eq!(formulae[0].versions.stable, "3.0");
    assert_eq!(formulae[1].versions.stable, "2.0");
    assert_eq!(index.casks()[0].token, "new");
    assert_eq!(index.casks()[0].version, "2.0");
    fs::remove_file(tap.path.join("Casks/new.rb"))
        .await
        .unwrap();
    assert!(store.refresh(&tap).await.unwrap().casks().is_empty());
}

#[tokio::test]
async fn reuses_only_matching_hashes_and_rebuilds_on_version_change() {
    let (_tmp, tap, store) = fixture();
    let _lock = store.lock().await.unwrap();
    let path = tap.path.join("Formula/keep.rb");
    fs::write(&path, formula("1.0")).await.unwrap();
    let mut old = store.refresh(&tap).await.unwrap();
    // A sentinel in the parsed entry distinguishes reuse from re-parsing.
    old.entries
        .get_mut(&path)
        .unwrap()
        .formula
        .as_mut()
        .unwrap()
        .desc = Some("reused".into());
    store.save(&old).await.unwrap();
    fs::write(tap.path.join("Formula/add.rb"), formula("2.0"))
        .await
        .unwrap();
    let reused = store.refresh(&tap).await.unwrap();
    assert_eq!(
        reused.entries[&path]
            .formula
            .as_ref()
            .unwrap()
            .desc
            .as_deref(),
        Some("reused")
    );
    old.version = INDEX_VERSION + 1;
    store.save(&old).await.unwrap();
    let rebuilt = store.refresh(&tap).await.unwrap();
    assert_eq!(
        rebuilt.entries[&path]
            .formula
            .as_ref()
            .unwrap()
            .desc
            .as_deref(),
        Some("Fixture formula")
    );
    fs::write(&path, formula("3.0")).await.unwrap();
    assert_eq!(
        store.refresh(&tap).await.unwrap().entries[&path]
            .formula
            .as_ref()
            .unwrap()
            .versions
            .stable,
        "3.0"
    );
}

#[tokio::test]
async fn corrupt_cache_recovers_and_failed_read_preserves_last_snapshot() {
    let (_tmp, tap, store) = fixture();
    let _lock = store.lock().await.unwrap();
    let path = tap.path.join("Formula/example.rb");
    fs::write(&path, formula("1.0")).await.unwrap();
    store.refresh(&tap).await.unwrap();
    fs::write(&store.index_path, b"incomplete JSON")
        .await
        .unwrap();
    assert_eq!(store.refresh(&tap).await.unwrap().formulae().len(), 1);
    let bytes = fs::read(&store.index_path).await.unwrap();
    fs::write(&path, [0xff, 0xfe]).await.unwrap();
    assert_eq!(store.refresh(&tap).await.unwrap().formulae().len(), 1);
    assert_eq!(fs::read(&store.index_path).await.unwrap(), bytes);
    fs::write(tap.path.join("Formula/latin1.rb"), [0xff, 0xfe])
        .await
        .unwrap();
    assert_eq!(store.refresh(&tap).await.unwrap().formulae().len(), 1);
    fs::remove_dir_all(&tap.path).await.unwrap();
    assert!(store.refresh(&tap).await.is_err());
    assert_eq!(fs::read(&store.index_path).await.unwrap(), bytes);
}

#[tokio::test]
async fn local_file_and_root_layout_match_current_loaders() {
    let (tmp, mut tap, store) = fixture();
    fs::write(tap.path.join("Formula/ignored.txt"), formula("1.0"))
        .await
        .unwrap();
    fs::create_dir_all(tap.path.join("Formula/nested"))
        .await
        .unwrap();
    fs::write(tap.path.join("Formula/nested/ignored.rb"), formula("1.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Formula/cask.rb"), cask("1.0"))
        .await
        .unwrap();
    let _lock = store.lock().await.unwrap();
    assert!(store.refresh(&tap).await.unwrap().formulae().is_empty());
    fs::remove_dir_all(tap.path.join("Formula")).await.unwrap();
    fs::write(tap.path.join("root.rb"), formula("1.0"))
        .await
        .unwrap();
    assert_eq!(
        store.refresh(&tap).await.unwrap().formulae()[0].name,
        "root"
    );
    let single = tmp.path().join("single.rb");
    fs::write(&single, formula("2.0")).await.unwrap();
    tap.path = single.clone();
    tap.kind = TapKind::LocalFile {
        path: single.clone(),
    };
    let index = store.refresh(&tap).await.unwrap();
    assert_eq!(index.formulae()[0].name, "single");
    assert_eq!(index.formulae()[0].rb_path.as_ref(), Some(&single));
    assert!(index.casks().is_empty());
    assert!(index.indexed_commit.is_none());
}

#[tokio::test]
async fn complete_manifest_has_no_compare_api_file_limit() {
    let (_tmp, tap, store) = fixture();
    let _lock = store.lock().await.unwrap();
    for n in 0..350 {
        fs::write(tap.path.join(format!("Formula/item{n}.rb")), formula("1.0"))
            .await
            .unwrap();
    }
    assert_eq!(store.refresh(&tap).await.unwrap().formulae().len(), 350);
    for n in 0..350 {
        fs::write(tap.path.join(format!("Formula/item{n}.rb")), formula("2.0"))
            .await
            .unwrap();
    }
    let index = store.refresh(&tap).await.unwrap();
    assert_eq!(index.formulae().len(), 350);
    assert!(index.formulae().iter().all(|f| f.versions.stable == "2.0"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_writers_use_one_snapshot_and_lock_survives_invalidation() {
    let (tmp, tap, store) = fixture();
    fs::write(tap.path.join("Formula/example.rb"), formula("1.0"))
        .await
        .unwrap();
    let lock = store.lock().await.unwrap();
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&store.lock_path)
        .unwrap();
    assert!(contender.try_lock().is_err());
    drop(lock);
    let root = tmp.path().join("cache");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let tap = tap.clone();
        let root = root.clone();
        tasks.spawn(async move {
            let store = TapIndexStore::new(&root, &tap.full_name);
            let _lock = store.lock().await.unwrap();
            store.refresh(&tap).await.unwrap().signature().unwrap()
        });
    }
    let first = tasks.join_next().await.unwrap().unwrap();
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), first);
    }
    store.invalidate().await.unwrap();
    let _lock = store.lock().await.unwrap();
    assert!(contender.try_lock().is_err());
    assert_eq!(store.refresh(&tap).await.unwrap().formulae().len(), 1);
    assert!(fs::read_dir(store.index_path.parent().unwrap())
        .await
        .unwrap()
        .next_entry()
        .await
        .unwrap()
        .is_some());
}

#[test]
fn tap_cache_names_do_not_alias() {
    let root = Path::new("cache");
    assert_ne!(
        TapIndexStore::new(root, "a-b/c").index_path,
        TapIndexStore::new(root, "a/b-c").index_path
    );
}

#[tokio::test]
async fn incremental_index_matches_existing_formula_and_cask_loaders() {
    let (_tmp, tap, store) = fixture();
    fs::write(tap.path.join("Formula/example.rb"), formula("1.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Formula/skipped.rb"), "invalid Ruby fixture")
        .await
        .unwrap();
    fs::write(tap.path.join("Casks/example.rb"), cask("1.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Casks/ignored.rb"), formula("1.0"))
        .await
        .unwrap();
    let manager = TapManager::new().unwrap();
    let _lock = store.lock().await.unwrap();
    let index = store.refresh(&tap).await.unwrap();
    assert_eq!(index.formulae().len(), 1);
    assert_eq!(index.casks().len(), 1);
    assert_eq!(
        serde_json::to_value(index.formulae()).unwrap(),
        serde_json::to_value(manager.load_formulae_from_tap(&tap).await.unwrap()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(index.casks()).unwrap(),
        serde_json::to_value(manager.load_casks_from_tap(&tap).await.unwrap()).unwrap()
    );
}

#[tokio::test]
async fn sharded_letter_directories_are_indexed() {
    let (_tmp, tap, store) = fixture();
    fs::create_dir_all(tap.path.join("Casks/b")).await.unwrap();
    fs::create_dir_all(tap.path.join("Formula/e"))
        .await
        .unwrap();
    fs::write(tap.path.join("Casks/b/example.rb"), cask("1.0"))
        .await
        .unwrap();
    fs::write(tap.path.join("Formula/e/example.rb"), formula("1.0"))
        .await
        .unwrap();
    let _lock = store.lock().await.unwrap();
    let index = store.refresh(&tap).await.unwrap();
    assert_eq!(index.casks().len(), 1);
    assert_eq!(index.formulae().len(), 1);
    let manager = TapManager::for_test(tap.clone(), tap.path.join("taps.json"));
    assert_eq!(manager.load_casks_from_tap(&tap).await.unwrap().len(), 1);
    assert_eq!(manager.load_formulae_from_tap(&tap).await.unwrap().len(), 1);
}
