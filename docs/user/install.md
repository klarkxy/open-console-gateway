[简体中文](install.zh-CN.md)

# Install And First Run

> Historical scope: published desktop installers of the `open-console-gateway` generation. Not the current `ocg` package. This page does not define an installer for this generation, and it does not record a test or a completed migration.

This generation is the headless `ocg` CLI (`ocg.exe` on Windows, `ocg` on Linux and macOS). `ocg3` and `open-console-gateway` are two generation branches of one project; the product name stays Open Console Gateway. Execution is one local CPA owned by that process. Custom HTTP providers remain provider routes on that local CPA. The default data root is `~/.ocg3`. A launch does not open, copy, move, delete, or adopt `~/.ocg-mgr` or `~/.ocg-mgr-cli`. The GPUI/Ely GUI is postponed. Whole CLI acceptance is pending. Commands are in the [CLI guide](cli.md). Design is in the [architecture](../architecture.md).

The NSIS, DMG, deb, and AppImage steps below install that generation's desktop app and its browser dashboard. Its data directory is `~/.ocg-mgr` (Windows: `%USERPROFILE%\.ocg-mgr`). That directory is not `~/.ocg3`. The steps do not move or delete an existing data directory unless the uninstall confirm page's **Delete application data** is chosen. Silent uninstalls and in-app updates of that app never deleted it.

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
