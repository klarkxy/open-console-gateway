//! Native startup recovery UI for the desktop host.
//!
//! Used when the desktop detects an older OCG background process still
//! occupying the gateway port: ask the user for consent before cleanup and
//! surface actionable startup errors when no tray/webview can be shown yet.

/// Ask the user whether the older OCG background process (identified by
/// `pid` and `image`) that occupies `port` may be stopped so the desktop
/// startup can be retried. Defaults to "No"; returns true only on "Yes".
pub fn confirm_cleanup(port: u16, pid: u32, image: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        imp::confirm_cleanup(port, pid, image)
    }
    #[cfg(not(windows))]
    {
        eprintln!(
            "ocg-manager: older OCG background process (pid {pid}, image {}) occupies port {port}; \
             stop it and retry desktop startup? (non-interactive fallback: no)",
            image.display()
        );
        false
    }
}

/// Latest release download page, opened when the user accepts the
/// newer-schema update prompt.
pub const RELEASES_PAGE_URL: &str =
    "https://github.com/klarkxy/open-console-gateway/releases/latest";

/// Ask the user whether to update now after startup was blocked by a data
/// directory written by a newer version. Defaults to "Yes"; returns true only
/// on "Yes".
pub fn prompt_update_for_newer_schema(found: i32, supported: i32) -> bool {
    #[cfg(windows)]
    {
        imp::prompt_update_for_newer_schema(found, supported)
    }
    #[cfg(not(windows))]
    {
        eprintln!(
            "ocg-manager: the data directory was written by a newer version (data version \
             {found}); this build supports up to data version {supported}. Download the latest \
             release: {RELEASES_PAGE_URL} (non-interactive fallback: no)"
        );
        false
    }
}

/// Confirm downloading and installing version `version`. Defaults to "Yes";
/// returns true only on "Yes".
pub fn confirm_install_update(version: &str) -> bool {
    #[cfg(windows)]
    {
        imp::confirm_install_update(version)
    }
    #[cfg(not(windows))]
    {
        eprintln!(
            "ocg-manager: version {version} is available; download and install it now? \
             (non-interactive fallback: no)"
        );
        false
    }
}

/// Show a failed automatic update; the caller opens the release page next.
pub fn show_update_error(error: &str) {
    #[cfg(windows)]
    {
        imp::show_update_error(error)
    }
    #[cfg(not(windows))]
    {
        eprintln!("ocg-manager update error: {error}");
    }
}

/// Open the release download page in the system browser. Non-Windows hosts
/// already print the URL from `prompt_update_for_newer_schema`.
pub fn open_release_page() {
    #[cfg(windows)]
    {
        imp::open_release_page()
    }
}

/// Show a visible, actionable startup error when the desktop cannot start.
pub fn show_startup_error(error: &str) {
    #[cfg(windows)]
    {
        imp::show_startup_error(error)
    }
    #[cfg(not(windows))]
    {
        eprintln!("ocg-manager startup error: {error}");
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON1, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONQUESTION, MB_ICONWARNING,
        MB_SETFOREGROUND, MB_TASKMODAL, MB_TOPMOST, MB_YESNO, MessageBoxW, SW_SHOW,
    };

    const TITLE: &str = "Open Console Gateway";

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(once(0)).collect()
    }

    fn message_box(text: &str, flags: u32) -> i32 {
        let title = wide(TITLE);
        let text = wide(text);
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags) }
    }

    pub fn confirm_cleanup(port: u16, pid: u32, image: &Path) -> bool {
        let text = format!(
            "检测到旧版本的 OCG 后台服务占用端口 {port}，桌面端无法启动。\r\n\r\n\
             进程 PID：{pid}\r\n\
             程序路径：{}\r\n\r\n\
             是否停止旧服务并重启桌面端？\r\n\r\n\
             该进程正在处理的请求会中断，账号、配置和数据不会被删除。",
            image.display()
        );
        let answer = message_box(
            &text,
            MB_YESNO
                | MB_DEFBUTTON2
                | MB_ICONQUESTION
                | MB_TASKMODAL
                | MB_SETFOREGROUND
                | MB_TOPMOST,
        );
        answer == IDYES
    }

    pub fn prompt_update_for_newer_schema(found: i32, supported: i32) -> bool {
        let text = format!(
            "本机数据由更新版本的 Open Console Gateway 写入（数据版本 {found}），当前版本最高支持数据版本 {supported}，无法启动。\r\n\r\n\
             旧版本继续打开可能损坏数据，因此启动已被阻止；账号、配置和数据不会被修改。\r\n\r\n\
             是否立即更新到最新版本？\r\n\r\n\
             「是」自动下载并安装（失败时打开发布页面）；「否」直接退出。"
        );
        let answer = message_box(
            &text,
            MB_YESNO
                | MB_DEFBUTTON1
                | MB_ICONWARNING
                | MB_TASKMODAL
                | MB_SETFOREGROUND
                | MB_TOPMOST,
        );
        answer == IDYES
    }

    pub fn confirm_install_update(version: &str) -> bool {
        let text = format!(
            "已连接到更新服务器，最新版本为 v{version}。\r\n\r\n\
             是否立即下载并安装？安装完成后新版本会自动打开。\r\n\r\n\
             「是」立即下载安装；「否」退出。"
        );
        let answer = message_box(
            &text,
            MB_YESNO
                | MB_DEFBUTTON1
                | MB_ICONQUESTION
                | MB_TASKMODAL
                | MB_SETFOREGROUND
                | MB_TOPMOST,
        );
        answer == IDYES
    }

    pub fn show_update_error(error: &str) {
        let text = format!(
            "自动更新失败。\r\n\r\n\
             错误详情：{error}\r\n\r\n\
             即将打开发布页面，也可以稍后手动下载最新版本。"
        );
        message_box(
            &text,
            MB_ICONERROR | MB_TASKMODAL | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }

    pub fn open_release_page() {
        let verb = wide("open");
        let url = wide(super::RELEASES_PAGE_URL);
        // ShellExecuteW returns a legacy HINSTANCE; values <= 32 signal failure.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOW,
            )
        };
        if (result as usize) <= 32 {
            tracing::warn!(
                url = super::RELEASES_PAGE_URL,
                error = %std::io::Error::last_os_error(),
                "failed to open release page"
            );
        }
    }

    pub fn show_startup_error(error: &str) {
        let text = format!(
            "Open Console Gateway 桌面端启动失败。\r\n\r\n\
             错误详情：{error}\r\n\r\n\
             请解决问题（例如停止冲突的 OCG 后台服务）后重新启动 \
             Open Console Gateway。"
        );
        message_box(
            &text,
            MB_ICONERROR | MB_TASKMODAL | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}
