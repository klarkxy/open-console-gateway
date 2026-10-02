use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, menu::Menu, menu::MenuItem};
use tauri_plugin_shell::ShellExt;

pub fn open_dashboard(app: &AppHandle) {
    let state = app.state::<crate::state::AppState>();
    let gateway_url = || {
        format!(
            "http://127.0.0.1:{}/dashboard/",
            state.core.active_gateway_port()
        )
    };
    let url = if cfg!(debug_assertions) {
        app.config()
            .build
            .dev_url
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(gateway_url)
    } else {
        gateway_url()
    };
    #[allow(deprecated)]
    let opened = app.shell().open(url, None);
    if let Err(_error) = opened {
        state
            .core
            .log_runtime_event("error", "dashboard", "failed to open dashboard");
    }
}

const TRAY_ID: &str = "main";

pub fn setup_tray(app: &tauri::AppHandle) -> crate::Result<()> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_visible(true)?;
        return Ok(());
    }

    let open_i = MenuItem::with_id(app, "open", "打开管理界面", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_i, &quit_i])?;

    let icon = app
        .default_window_icon()
        .ok_or_else(|| anyhow::anyhow!("default window icon is missing"))?
        .clone();

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Open Console Gateway")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                open_dashboard(app);
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                open_dashboard(app);
            }
        })
        .build(app)?;

    Ok(())
}
