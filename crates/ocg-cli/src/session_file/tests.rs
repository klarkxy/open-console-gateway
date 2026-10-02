use super::{SessionStore, open_private, write_private};
use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-cli-session-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cookie(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append(SET_COOKIE, HeaderValue::from_str(value).unwrap());
    headers
}

#[test]
fn session_tracks_set_cookie_deletion_and_dashboard_path() {
    let dir = temp_dir();
    let path = dir.join("session.json");
    let endpoint = "http://127.0.0.1:9042";
    let mut store = SessionStore::load(&path, endpoint).unwrap();
    store
        .apply_set_cookie(&cookie(
            "ocg_dashboard_session=sekret; HttpOnly; Path=/dashboard",
        ))
        .unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("sekret"));
    assert!(saved.contains(endpoint));
    assert_eq!(
        store
            .header_for("/dashboard/api/v4/contract", false)
            .as_deref(),
        Some("ocg_dashboard_session=sekret")
    );
    assert!(store.header_for("/v1/models", false).is_none());
    assert!(store.header_for("/dashboard2", false).is_none());

    store
        .apply_set_cookie(&cookie(
            "ocg_dashboard_session=; Max-Age=0; Path=/dashboard",
        ))
        .unwrap();
    assert!(
        store
            .header_for("/dashboard/api/v4/contract", false)
            .is_none()
    );
    assert!(!std::fs::read_to_string(&path).unwrap().contains("sekret"));

    let mut secure = SessionStore::load(&dir.join("https.json"), "https://127.0.0.1:9042").unwrap();
    secure
        .apply_set_cookie(&cookie("ocg_dashboard_session=sekret; Secure; Path=/"))
        .unwrap();
    assert!(
        secure
            .header_for("/dashboard/api/v4/contract", false)
            .is_none()
    );
    assert!(
        secure
            .header_for("/dashboard/api/v4/contract", true)
            .is_some()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn session_file_rejects_a_different_endpoint() {
    let dir = temp_dir();
    let path = dir.join("session.json");
    write_private(
        &path,
        br#"{"version":1,"endpoint":"http://127.0.0.1:1","cookies":[]}"#,
    )
    .unwrap();
    let error = SessionStore::load(&path, "http://127.0.0.1:2")
        .err()
        .expect("a session file stays bound to its endpoint");
    assert!(error.to_string().contains("bound to"));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("127.0.0.1:1")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn private_output_file_is_created_without_replacing_a_symlink() {
    let dir = temp_dir();
    let path = dir.join("out.json");
    let file = open_private(&path).unwrap();
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(open_private(&link).is_err());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(windows)]
#[test]
fn credential_file_dacl_after_create_and_replacement() {
    let dir = temp_dir();
    let created = dir.join("created.json");
    drop(open_private(&created).unwrap());
    assert_private_dacl(&created);

    let replaced = dir.join("replaced.json");
    std::fs::write(&replaced, b"broad").unwrap();
    let inherited = dacl_report(&replaced);
    assert!(
        !inherited.is_private(),
        "a normal create must not already have the private DACL: {inherited:?}"
    );
    write_private(&replaced, b"secret-bytes").unwrap();
    assert_eq!(std::fs::read(&replaced).unwrap(), b"secret-bytes");
    assert_private_dacl(&replaced);
    write_private(&replaced, b"next-secret").unwrap();
    assert_eq!(std::fs::read(&replaced).unwrap(), b"next-secret");
    assert_private_dacl(&replaced);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct DaclReport {
    protected: bool,
    me: String,
    aces: Vec<DaclAce>,
}

#[cfg(windows)]
#[derive(Debug)]
struct DaclAce {
    sid: String,
    allow: bool,
    inherited: bool,
    inheritance_none: bool,
    propagation_none: bool,
    rights: String,
}

#[cfg(windows)]
impl DaclReport {
    pub(crate) fn is_private(&self) -> bool {
        self.protected
            && self.aces.len() == 2
            && self.aces.iter().all(|ace| {
                ace.allow
                    && !ace.inherited
                    && ace.inheritance_none
                    && ace.propagation_none
                    && rights_are_full(&ace.rights)
                    && (ace.sid == self.me || ace.sid == "S-1-5-18")
            })
            && self.aces.iter().any(|ace| ace.sid == self.me)
            && self.aces.iter().any(|ace| ace.sid == "S-1-5-18")
    }
}

#[cfg(windows)]
fn rights_are_full(rights: &str) -> bool {
    let compact = rights.replace(' ', "");
    compact.eq_ignore_ascii_case("FullControl")
        || compact.eq_ignore_ascii_case("GenericAll")
        || compact == "2032127"
        || compact == "268435456"
        || compact.to_ascii_lowercase().contains("fullcontrol")
}

#[cfg(windows)]
pub(crate) fn assert_private_dacl(path: &std::path::Path) {
    let report = dacl_report(path);
    assert!(
        report.is_private(),
        "private DACL missing for {}: {report:?}",
        path.display()
    );
}

#[cfg(windows)]
pub(crate) fn dacl_report(path: &std::path::Path) -> DaclReport {
    // FileInfo.GetAccessControl reads the real DACL. Get-Acl cannot autoload
    // its module when the test runner redirects stdin.
    let script = r#"
$ErrorActionPreference = 'Stop'
$item = New-Object System.IO.FileInfo $env:OCG_PRIVATE_FILE
$acl = $item.GetAccessControl()
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
Write-Output ('protected=' + $acl.AreAccessRulesProtected)
Write-Output ('me=' + $me)
foreach ($ace in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
  Write-Output ('ace=' + $ace.IdentityReference.Value + '|' + $ace.AccessControlType + '|' + $ace.IsInherited + '|' + $ace.InheritanceFlags + '|' + $ace.PropagationFlags + '|' + $ace.FileSystemRights)
}
"#;
    let mut powershell = std::path::PathBuf::from(
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string()),
    );
    powershell.push("System32");
    powershell.push("WindowsPowerShell");
    powershell.push("v1.0");
    powershell.push("powershell.exe");
    let output = std::process::Command::new(powershell)
        .env("OCG_PRIVATE_FILE", path)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("Windows PowerShell DACL read");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "DACL read failed: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut protected = None;
    let mut me = None;
    let mut aces = Vec::new();
    for line in stdout.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if let Some(value) = line.strip_prefix("protected=") {
            protected = Some(value.eq_ignore_ascii_case("True"));
        } else if let Some(value) = line.strip_prefix("me=") {
            me = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("ace=") {
            let mut parts = value.splitn(6, '|');
            aces.push(DaclAce {
                sid: parts.next().unwrap_or_default().to_string(),
                allow: parts
                    .next()
                    .unwrap_or_default()
                    .eq_ignore_ascii_case("Allow"),
                inherited: parts
                    .next()
                    .unwrap_or_default()
                    .eq_ignore_ascii_case("True"),
                inheritance_none: parts
                    .next()
                    .unwrap_or_default()
                    .eq_ignore_ascii_case("None"),
                propagation_none: parts
                    .next()
                    .unwrap_or_default()
                    .eq_ignore_ascii_case("None"),
                rights: parts.next().unwrap_or_default().to_string(),
            });
        }
    }
    DaclReport {
        protected: protected.expect("DACL read did not report protection"),
        me: me.expect("DACL read did not report the current user"),
        aces,
    }
}
