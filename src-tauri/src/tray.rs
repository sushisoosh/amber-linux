//! The tray icon: Amber's home while no window is open.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let dictate = MenuItem::with_id(app, "dictate", "Start / stop dictation", true, None::<&str>)?;
    let bubble = MenuItem::with_id(app, "bubble", "Show / hide bubble", true, None::<&str>)?;
    let copy_last = MenuItem::with_id(app, "copy-last", "Copy last dictation", true, None::<&str>)?;
    let again = MenuItem::with_id(app, "again", "Recognize again", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Amber", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let menu = if cfg!(target_os = "linux") {
        Menu::with_items(app, &[&dictate, &sep, &copy_last, &again, &sep3, &settings, &sep2, &quit])?
    } else {
        Menu::with_items(app, &[&dictate, &bubble, &sep, &copy_last, &again, &sep3, &settings, &update, &sep2, &quit])?
    };

    let mut tray = TrayIconBuilder::with_id("amber")
        .tooltip("Amber - dictation")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "dictate" => crate::dictation::toggle(app),
            "bubble" => crate::hotkeys::toggle_bubble(app),
            "copy-last" => copy_last_dictation(app),
            "again" => {
                // Recognition may take a while; the menu must not wait for it.
                let app = app.clone();
                std::thread::spawn(move || match crate::dictation::recognize_again(&app) {
                    Ok(_) => crate::notify(&app, "Amber", if cfg!(target_os = "linux") { "Recognized again - review and copy the result in the recorder." } else { "Recognized again - the text is on the clipboard." }),
                    Err(e) => crate::notify(&app, "Amber", &e),
                });
            }
            "settings" => crate::bubble::open_settings(app, None),
            "update" => crate::bubble::open_settings(app, Some("app")),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                crate::bubble::open_settings(tray.app_handle(), None);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

pub fn sync_state(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id("amber") else { return };
        let Some(idle) = handle.default_window_icon() else { return };
        // Read the current state here so a queued update cannot restore a stale color.
        let recording = handle.state::<crate::state::Core>().dictation.is_recording();
        let icon = if recording {
            tauri::image::Image::new_owned(
                amber_core::indicator::recording_rgba(idle.rgba().to_vec()),
                idle.width(), idle.height(),
            )
        } else {
            idle.clone()
        };
        if let Err(e) = tray.set_icon(Some(icon)) {
            crate::diag::warn(format!("Could not update tray icon: {e}"));
        }
        let _ = tray.set_tooltip(Some(if recording { "Amber - recording" } else { "Amber - dictation" }));
    });
}

fn copy_last_dictation(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    if let Some(result) = app.state::<crate::state::Core>().dictation.result() {
        match crate::insert::to_clipboard(&result.text) {
            Ok(()) => crate::notify(app, "Amber", "The last dictation is on the clipboard."),
            Err(e) => crate::notify(app, "Amber", &format!("Could not copy: {e}")),
        }
        return;
    }
    let latest = app.state::<crate::state::Core>().memory.latest();
    match latest {
        Some(entry) => match crate::insert::to_clipboard(&entry.text) {
            Ok(()) => crate::notify(app, "Amber", "The last dictation is on the clipboard."),
            Err(e) => crate::notify(app, "Amber", &format!("Could not copy: {e}")),
        },
        None => crate::notify(app, "Amber", "No dictation in the history yet."),
    }
}
