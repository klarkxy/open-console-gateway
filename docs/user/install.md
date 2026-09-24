[简体中文](install.zh-CN.md)

# Install And First Run

Install the local gateway, launch it, and close the browser tab when you're
done. The rest is mostly convincing your OS that small developers exist.

## Windows 10/11 x64

1. Run the NSIS setup `ocg-manager_<version>_windows-x64-setup.exe`. It
   installs for the current user without administrator rights.
2. Launch **Open Console Gateway** from the Start menu. The dashboard opens in your
   system browser; use the tray icon to open it again later.
3. Current Windows builds are unsigned, so SmartScreen may warn. Click
   **More info → Run anyway** to continue.
4. Add an OpenCode-Go account in the **Accounts** view, copy the Key,
   and point your client at `http://127.0.0.1:9042/v1`.
5. Running the installer again replaces the existing copy in place and keeps
   `%USERPROFILE%\.ocg-mgr`. Uninstall from Windows **Installed apps**. The
   confirm page includes **Delete application data**; leave it unchecked to
   keep the data directory. Silent uninstalls and in-app updates never delete
   it.

## macOS 11+ Intel / Apple Silicon

1. Open the Universal DMG and drag **Open Console Gateway** to **Applications**.
2. The app is ad-hoc signed, so the first launch may be blocked. Open
   **Privacy & Security** and click **Open Anyway**.
3. Launch the app. The dashboard opens in your system browser; use the tray
   icon to reopen it. Add an account, copy the Key, and configure
   your client.

## Linux x64

1. Verify the download against `SHA256SUMS` first.
2. Install the `.deb` with your package manager, or mark the AppImage
   executable with `chmod +x ocg-manager_<version>_linux-x64.AppImage`.
3. Launch the executable. The dashboard opens in your system browser; use the
   tray icon to reopen it.
4. Data lives in `~/.ocg-mgr/`.

If you enable auto-start on Windows, the app resumes from the tray and leaves the browser closed.

Release desktop builds bundle the `ocg-manager` Codex skill. On the first
successful app launch after installation or upgrade, they synchronize it to
`~/.agents/skills/ocg-manager` (Windows: `%USERPROFILE%\.agents\skills\ocg-manager`).
An older OCG-managed copy is backed up under `~/.agents/skill-backups/` when the bundled skill changes;
an unrelated same-name skill is left unchanged. The installer alone does not
run this step when the app has not yet launched. Development builds do not
auto-install the skill.

---

[User guide index](../USER.md) · [简体中文](install.zh-CN.md) · [Docs index](../README.md)
