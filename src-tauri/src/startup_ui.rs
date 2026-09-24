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

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TASKMODAL,
        MB_TOPMOST, MB_YESNO, MessageBoxW,
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
