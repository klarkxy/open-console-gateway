use super::{SkillSyncStatus, sync_at};
use std::fs;
use std::path::PathBuf;

fn temp_home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("ocg-skill-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn installs_and_skips_identical_bundled_skill() {
    let home = temp_home();
    let first = sync_at(&home).unwrap();
    assert_eq!(first.status, SkillSyncStatus::Installed);
    assert!(first.path.join("SKILL.md").is_file());
    assert!(first.path.join("references/secrets.md").is_file());

    let second = sync_at(&home).unwrap();
    assert_eq!(second.status, SkillSyncStatus::UpToDate);
    assert_eq!(first.path, second.path);
    assert_eq!(
        fs::read_dir(home.join(".agents/skill-backups")).is_err(),
        true
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn backs_up_a_modified_managed_skill_before_updating() {
    let home = temp_home();
    let installed = sync_at(&home).unwrap();
    fs::write(installed.path.join("SKILL.md"), "user customization\n").unwrap();
    fs::write(
        installed.path.join(".ocg-managed"),
        "ocg-manager\nolder-version\nolder-digest\n",
    )
    .unwrap();

    let updated = sync_at(&home).unwrap();
    assert_eq!(updated.status, SkillSyncStatus::Updated);
    assert_ne!(
        fs::read_to_string(updated.path.join("SKILL.md")).unwrap(),
        "user customization\n"
    );
    let backups = home.join(".agents/skill-backups");
    let backup = fs::read_dir(&backups)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read_to_string(backup.join("SKILL.md")).unwrap(),
        "user customization\n"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn leaves_current_version_user_edits_unchanged() {
    let home = temp_home();
    let installed = sync_at(&home).unwrap();
    fs::write(installed.path.join("SKILL.md"), "local edit\n").unwrap();

    let error = sync_at(&home).unwrap_err();
    assert!(error.to_string().contains("local edits"));
    assert_eq!(
        fs::read_to_string(installed.path.join("SKILL.md")).unwrap(),
        "local edit\n"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn leaves_an_unmanaged_same_name_skill_unchanged() {
    let home = temp_home();
    let existing = home.join(".agents/skills/ocg-manager");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("SKILL.md"), "user-owned\n").unwrap();

    let error = sync_at(&home).unwrap_err();
    assert!(error.to_string().contains("not OCG-managed"));
    assert_eq!(
        fs::read_to_string(existing.join("SKILL.md")).unwrap(),
        "user-owned\n"
    );
    fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_skill_parent() {
    use std::os::unix::fs::symlink;

    let home = temp_home();
    let elsewhere = temp_home();
    fs::create_dir(home.join(".agents")).unwrap();
    symlink(&elsewhere, home.join(".agents/skills")).unwrap();
    assert!(sync_at(&home).is_err());
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    fs::remove_dir_all(home).unwrap();
    fs::remove_dir_all(elsewhere).unwrap();
}
