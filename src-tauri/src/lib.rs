//! Amber desktop dictation, locally or through an explicitly selected server.

mod autostart;
mod bubble;
mod capture;
mod commands;
mod connections;
mod control;
mod diag;
mod dictation;
mod hotkeys;
mod insert;
mod imports;
#[cfg(target_os = "linux")]
mod linux_desktop;
mod llm;
mod memory;
mod mute;
mod phone;
mod process;
mod secrets;
mod state;
mod testing;
mod tray;
mod tunnel;
mod updates;
#[cfg_attr(target_os = "linux", path = "whisper_linux.rs")]
mod whisper;

use amber_core::local::Layout;
use amber_core::settings::EngineKind;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use state::{Core, Paths};

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // The single-instance plugin already delivered this action if Amber was running.
    // A cold cancellation must not start a recorder window or block a lock-screen wrapper.
    #[cfg(target_os = "linux")]
    if std::env::args().any(|arg| arg == "--cancel") {
        app.exit(0);
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    let background_control = amber_core::autostart::background_control(&std::env::args().collect::<Vec<_>>());
    // Capture before creating any UI, including on a cold desktop-shortcut launch.
    #[cfg(target_os = "linux")]
    let launch_target = if background_control { insert::remember() } else { None };
    let config = app.path().app_config_dir()?;
    let data = app.path().app_local_data_dir()?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&config, &data] {
            std::fs::create_dir_all(dir)?;
            if std::fs::symlink_metadata(dir)?.file_type().is_symlink() {
                return Err("Amber's private data directory must not be a symbolic link".into());
            }
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let paths = Paths {
        settings: config.join("settings.json"),
        secrets: config.join("secrets.bin"),
        logs: data.join("logs"),
        layout: Layout { root: data },
    };
    diag::init(&paths.logs);
    amber_core::engine::log_languages_to(diag::info);
    diag::info(format!("Amber {} starting", env!("CARGO_PKG_VERSION")));
    app.manage(Core::new(paths));
    testing::load_at_start(app)?;

    let core = app.state::<Core>();
    let settings = core.settings();
    #[cfg(not(target_os = "linux"))]
    bubble::create(app)?;
    #[cfg(target_os = "linux")]
    linux_desktop::init(app);
    #[cfg(target_os = "linux")]
    if let Err(e) = bubble::create(app) {
        diag::warn(format!("Recording indicator unavailable: {e}"));
    }
    if let Err(e) = tray::create(app) {
        #[cfg(not(target_os = "linux"))]
        return Err(e.into());
        #[cfg(target_os = "linux")]
        diag::warn(format!("Tray unavailable; use the recorder window: {e}"));
    }
    if let Err(e) = hotkeys::register(app, &settings) {
        diag::warn(format!("hotkey: {e}"));
        notify(app, "Amber: hotkey unavailable", &e);
    }
    autostart::sync_at_start(app);

    // Speech model first, cleanup model after it: loading both at once could leave a smaller
    // graphics card without room for Whisper - and then dictation itself would break.
    let load_whisper = settings.engine == EngineKind::Local && settings.local.keep_loaded;
    let load_llm = llm::wants_local(&settings);
    if load_whisper || load_llm {
        let app = app.clone();
        std::thread::spawn(move || {
            let core = app.state::<Core>();
            if load_whisper {
                let _ = core.whisper.ensure(&app);
            }
            if load_llm {
                core.llm.warm(&app, true);
            }
        });
    }

    // An explicitly configured login start stays in the background after onboarding.
    let autostarted = std::env::args().any(|a| a == amber_core::autostart::ARG);
    #[cfg(not(target_os = "linux"))]
    let background_control = false;
    if background_control && !settings.onboarded {
        notify(app, "Amber", "Open Amber from your application launcher to complete setup before dictating.");
    } else if !background_control && !settings.onboarded {
        bubble::open_settings(app, Some("welcome"));
    } else if !background_control && !autostarted {
        bubble::open_settings(app, amber_core::autostart::settings_page(&std::env::args().collect::<Vec<_>>()).as_deref());
    }
    #[cfg(not(target_os = "linux"))]
    control::start(app);
    #[cfg(not(target_os = "linux"))]
    if settings.phone.enabled {
        let app = app.clone();
        std::thread::spawn(move || {
            phone::sync(&app);
            tunnel::sync(&app);
        });
    }
    #[cfg(not(target_os = "linux"))]
    if let Some(path) = tunnel::import_path(&std::env::args().collect::<Vec<_>>()) {
        tunnel::import(app, &path);
    }
    #[cfg(not(target_os = "linux"))]
    updates::spawn(app);
    #[cfg(target_os = "linux")]
    if settings.onboarded && std::env::args().any(|a| a == "--toggle") {
        dictation::start_after_desktop_ready(app, launch_target);
    }
    Ok(())
}

pub fn run() {
    #[cfg(target_os = "linux")]
    unsafe { libc::umask(0o077); }
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // First: a second start only brings the running Amber forward - or hands it a tunnel
        // setup (tools/setup-tunnel.sh).
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            #[cfg(target_os = "linux")]
            {
                if args.iter().any(|a| a == "--cancel") {
                    dictation::cancel(app);
                } else if args.iter().any(|a| a == "--insert-last") {
                    dictation::insert_last_result(app);
                } else if args.iter().any(|a| a == "--toggle") {
                    dictation::toggle(app);
                } else {
                    bubble::open_settings(app, amber_core::autostart::settings_page(&args).as_deref());
                }
            }
            #[cfg(not(target_os = "linux"))]
            match tunnel::import_path(&args) {
                Some(path) => tunnel::import(app, &path),
                None => bubble::open_settings(app, amber_core::autostart::settings_page(&args).as_deref()),
            }
        }));
    #[cfg(not(target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build());
    #[cfg(target_os = "linux")]
    let builder = if linux_desktop::use_x11_shortcuts() {
        builder.plugin(tauri_plugin_global_shortcut::Builder::new().build())
    } else {
        builder
    };
    builder
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| setup(app.handle()))
        .invoke_handler(tauri::generate_handler![
            commands::get_overview,
            commands::get_desktop_status,
            commands::get_storage_status,
            commands::get_last_result,
            commands::clear_last_result,
            commands::cancel_dictation,
            commands::get_testing_state,
            commands::start_test_recording,
            commands::replay_test_recording,
            commands::stop_test_recording,
            commands::cancel_test_recording,
            commands::clear_test_results,
            commands::save_test_table,
            commands::delete_test_table,
            commands::reload_test_tables,
            commands::quit_app,
            commands::save_settings,
            commands::set_api_key,
            commands::has_api_key,
            commands::test_server,
            commands::list_microphones,
            commands::mic_test,
            commands::local_info,
            commands::download,
            commands::cancel_download,
            commands::remove_download,
            commands::start_local,
            commands::stop_local,
            connections::has_connection_key,
            connections::set_connection_key,
            connections::test_connection,
            connections::discover_connection_models,
            imports::browse_local_model,
            imports::import_local_model,
            imports::cancel_model_import,
            imports::remove_imported_model,
            imports::browse_cleanup_model,
            imports::import_cleanup_model,
            imports::cancel_cleanup_model_import,
            imports::remove_imported_cleanup_model,
            imports::setup_onnx_runtime,
            imports::cancel_onnx_runtime_setup,
            commands::bubble_pointer,
            commands::bubble_config,
            commands::open_settings,
            commands::toggle_dictation,
            commands::get_log,
            commands::check_update,
            commands::install_update,
            commands::open_url,
            commands::phone_info,
            commands::phone_new_key,
            commands::phone_remove_key,
            commands::start_llm,
            commands::cleanup_preview,
            commands::chat_providers,
            commands::running_programs,
            commands::history_list,
            commands::history_comparisons,
            commands::history_delete,
            commands::history_clear,
            commands::copy_text,
            commands::recognize_again,
            commands::stats_summary,
            commands::stats_clear,
            commands::set_tunnel_token,
        ])
        .build(tauri::generate_context!())
        .expect("Amber failed to start")
        .run(|_app, event| match event {
            // Closing the settings window keeps Amber running in the tray; only "Quit" ends it.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // Never leave the user muted in Discord.
            tauri::RunEvent::Exit => {
                mute::release_now();
                #[cfg(target_os = "linux")]
                linux_desktop::shutdown();
            }
            _ => {}
        });
}
