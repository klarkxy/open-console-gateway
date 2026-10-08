//! Trusted executable discovery and bounded official CLI execution.
use super::*;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct Installation {
    pub(super) info: CopilotInstallation,
    pub(super) cli: Option<PathBuf>,
    pub(super) scheme: String,
    pub(super) portable: Option<PathBuf>,
}
pub(super) trait Runner: Send + Sync {
    fn run(&self, installation: &Installation, args: &[String]) -> ByokResult<String>;
}
pub(super) struct ProcessRunner;
impl Runner for ProcessRunner {
    fn run(&self, installation: &Installation, args: &[String]) -> ByokResult<String> {
        let mut command = Command::new(&installation.info.executable);
        if let Some(cli) = &installation.cli {
            command.arg(cli).env("ELECTRON_RUN_AS_NODE", "1");
        }
        if let Some(portable) = &installation.portable {
            command.env("VSCODE_PORTABLE", portable);
        } else {
            command.env_remove("VSCODE_PORTABLE");
        }
        command
            .env_remove("VSCODE_DEV")
            .env_remove("VSCODE_APPDATA")
            .env_remove("VSCODE_IPC_HOOK_CLI")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .map_err(|_| ByokError::precondition("Cannot start the selected VS Code CLI"))?;
        let (tx, rx) = mpsc::channel();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            thread::spawn(move || {
                let mut reader = stream;
                let mut bytes = Vec::new();
                let mut block = [0; 4096];
                let mut truncated = false;
                while let Ok(n) = reader.read(&mut block) {
                    if n == 0 {
                        break;
                    }
                    if bytes.len() + n <= 65536 {
                        bytes.extend_from_slice(&block[..n]);
                    } else {
                        truncated = true;
                    }
                }
                let _ = tx.send((bytes, truncated));
            });
        }
        drop(tx);
        let start = Instant::now();
        let timeout = if args.iter().any(|a| a == "--install-extension") {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(15)
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ByokError::internal("Cannot inspect VS Code CLI completion"));
                }
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ByokError::precondition(
                    "VS Code CLI timed out; inspect the selected target before retrying",
                ));
            }
            thread::sleep(Duration::from_millis(25));
        };
        let first = rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| ByokError::internal("VS Code CLI output did not close"))?;
        let second = rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| ByokError::internal("VS Code CLI output did not close"))?;
        if !status.success() {
            return Err(ByokError::precondition(
                "VS Code CLI failed; check its installation and selected Profile",
            ));
        }
        if first.1 || second.1 {
            return Err(ByokError::precondition(
                "VS Code CLI output exceeds the bounded limit",
            ));
        }
        // Both channels are combined; only exact IDs/version lines are interpreted.
        Ok(format!(
            "{}\n{}",
            String::from_utf8_lossy(&first.0),
            String::from_utf8_lossy(&second.0)
        ))
    }
}
pub(super) fn discover() -> Vec<Installation> {
    let home = crate::dsh_application_host::user_home();
    let mut result = vec![];
    #[cfg(windows)]
    {
        for (id, label, folder, exe, bin, scheme, data, extensions) in [
            (
                "stable",
                "VS Code",
                "Microsoft VS Code",
                "Code.exe",
                "code.cmd",
                "vscode",
                "Code",
                ".vscode",
            ),
            (
                "insiders",
                "VS Code Insiders",
                "Microsoft VS Code Insiders",
                "Code - Insiders.exe",
                "code-insiders.cmd",
                "vscode-insiders",
                "Code - Insiders",
                ".vscode-insiders",
            ),
        ] {
            let mut roots = vec![
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join("AppData/Local"))
                    .join("Programs")
                    .join(folder),
                PathBuf::from(
                    std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:/Program Files".into()),
                )
                .join(folder),
            ];
            for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                if directory.join(bin).is_file()
                    && directory
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.eq_ignore_ascii_case("bin"))
                    && let Some(root) = directory.parent()
                    && !roots.iter().any(|p| p == root)
                {
                    roots.push(root.to_path_buf());
                }
            }
            for root in roots {
                let executable = root.join(exe);
                if !executable.is_file() {
                    continue;
                }
                let mut cli = root.join("resources/app/out/cli.js");
                if !cli.is_file()
                    && let Ok(text) = fs::read_to_string(root.join("bin").join(bin))
                {
                    for part in text.split('"') {
                        if let Some(relative) = part.strip_prefix("%~dp0")
                            && relative.ends_with("resources\\app\\out\\cli.js")
                        {
                            let proposed = root.join("bin").join(relative);
                            if let Ok(p) = safe::canonical_lexical_path(&proposed)
                                && p.starts_with(&root)
                                && p.is_file()
                            {
                                cli = p;
                                break;
                            }
                        }
                    }
                }
                if !cli.is_file() {
                    continue;
                }
                let portable = std::env::var_os("VSCODE_PORTABLE")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute() && p.is_dir())
                    .or_else(|| root.join("data").is_dir().then(|| root.join("data")));
                let user_data = if let Some(portable) = &portable {
                    portable.join("user-data")
                } else {
                    std::env::var_os("VSCODE_APPDATA")
                        .or_else(|| std::env::var_os("APPDATA"))
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join("AppData/Roaming"))
                        .join(data)
                };
                let extension_dir = if let Some(portable) = &portable {
                    portable.join("extensions")
                } else {
                    home.join(extensions).join("extensions")
                };
                let version = cli
                    .parent()
                    .and_then(Path::parent)
                    .and_then(|p| package_version(&p.join("package.json")));
                result.push(Installation {
                    info: CopilotInstallation {
                        id: id.into(),
                        label: label.into(),
                        executable: executable.to_string_lossy().into(),
                        version,
                        user_data_dir: user_data.to_string_lossy().into(),
                        extensions_dir: extension_dir.to_string_lossy().into(),
                    },
                    cli: Some(cli),
                    scheme: scheme.into(),
                    portable,
                });
                break;
            }
        }
    }
    #[cfg(not(windows))]
    {
        for (id, label, program, scheme, data, extensions) in [
            ("stable", "VS Code", "code", "vscode", "Code", ".vscode"),
            (
                "insiders",
                "VS Code Insiders",
                "code-insiders",
                "vscode-insiders",
                "Code - Insiders",
                ".vscode-insiders",
            ),
        ] {
            let candidates = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join(program))
                .collect::<Vec<_>>();
            #[cfg(target_os = "macos")]
            let mut candidates = candidates;
            #[cfg(target_os = "macos")]
            candidates.push(PathBuf::from(format!(
                "/Applications/{}.app/Contents/Resources/app/bin/{program}",
                if id == "stable" {
                    "Visual Studio Code"
                } else {
                    "Visual Studio Code - Insiders"
                }
            )));
            if let Some(exe) = candidates.into_iter().find(|p| p.is_file()) {
                #[cfg(target_os = "macos")]
                let user_data = home.join("Library/Application Support").join(data);
                #[cfg(not(target_os = "macos"))]
                let user_data = std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"))
                    .join(data);
                let version = exe
                    .parent()
                    .and_then(Path::parent)
                    .and_then(|p| package_version(&p.join("package.json")))
                    .or_else(|| {
                        package_version(&PathBuf::from(if id == "stable" {
                            "/usr/share/code/resources/app/package.json"
                        } else {
                            "/usr/share/code-insiders/resources/app/package.json"
                        }))
                    });
                result.push(Installation {
                    info: CopilotInstallation {
                        id: id.into(),
                        label: label.into(),
                        executable: exe.to_string_lossy().into(),
                        version,
                        user_data_dir: user_data.to_string_lossy().into(),
                        extensions_dir: home
                            .join(extensions)
                            .join("extensions")
                            .to_string_lossy()
                            .into(),
                    },
                    cli: None,
                    scheme: scheme.into(),
                    portable: None,
                });
            }
        }
    }
    result
}

fn package_version(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}
