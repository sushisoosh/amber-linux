//! Opt-in login startup: Windows' Run key or a per-user XDG autostart desktop entry.
//! Desktop/Task Manager disable switches are respected, never user services or lingering.

#[cfg(windows)]
use amber_core::autostart::{disabled_in_task_manager, run_command, APPROVED_KEY, RUN_KEY, VALUE_NAME};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::Core;

/// Debug builds and release builds run straight from the build folder (`target\release`)
/// must not point Windows' autostart at themselves - only the installed app registers.
pub fn is_installed() -> bool {
    !cfg!(debug_assertions)
        && std::env::current_exe()
            .map(|p| !p.to_string_lossy().to_lowercase().contains(r"\target\") && !p.components().any(|part| part.as_os_str() == "target"))
            .unwrap_or(false)
}

/// At start: Task Manager's switch wins over the setting; otherwise the registry is made to
/// match the setting (which also moves an old entry to the current install path).
pub fn sync_at_start(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    {
        let core = app.state::<Core>();
        match linux::disabled_by_desktop() {
            Ok(true) => {
                if core.settings().autostart {
                    let after = core.update_settings(|s| s.autostart = false);
                    let _ = app.emit("settings-changed", after);
                }
                return;
            }
            Ok(false) => (),
            Err(e) => {
                crate::diag::warn(format!("autostart not changed: {e}"));
                return;
            }
        }
        if let Err(e) = set(core.settings().autostart) {
            crate::diag::warn(format!("autostart not changed: {e}"));
        }
    }
    #[cfg(windows)]
    {
        if !is_installed() {
            return;
        }
        let core = app.state::<Core>();
        if disabled_in_task_manager(reg::approved().as_deref()) {
            if core.settings().autostart {
                crate::diag::info("autostart was turned off in Task Manager");
                let after = core.update_settings(|s| s.autostart = false);
                let _ = app.emit("settings-changed", after);
            }
            return;
        }
        if let Err(e) = set(core.settings().autostart) {
            crate::diag::warn(format!("autostart not changed: {e}"));
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = app;
}

/// The user flipped the switch in Amber's settings.
pub fn set(on: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if on && !is_installed() {
            return Err("Login startup requires an installed release build, not a debug/target build".into());
        }
        return linux::set(on);
    }
    #[cfg(windows)]
    {
        if !is_installed() {
            return Ok(());
        }
        if on {
            std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| reg::enable(&run_command(&exe.to_string_lossy())))
        } else {
            reg::disable()
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = on;
        Err("Login startup is not supported on this platform".into())
    }
}

#[cfg(windows)]
mod reg {
    use super::*;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY,
    };

    fn check(e: WIN32_ERROR) -> Result<(), String> {
        if e == ERROR_SUCCESS || e == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("registry error {}", e.0))
        }
    }

    pub fn approved() -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        let mut len = buf.len() as u32;
        let e = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(APPROVED_KEY),
                &HSTRING::from(VALUE_NAME),
                RRF_RT_REG_BINARY,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        };
        (e == ERROR_SUCCESS).then(|| buf[..len as usize].to_vec())
    }

    pub fn enable(command: &str) -> Result<(), String> {
        let data: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                &HSTRING::from(VALUE_NAME),
                REG_SZ.0,
                Some(data.as_ptr() as *const _),
                (data.len() * 2) as u32,
            )
        })?;
        // Turning it on in Amber also clears an old "disabled" from Task Manager.
        check(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(APPROVED_KEY), &HSTRING::from(VALUE_NAME)) })
    }

    pub fn disable() -> Result<(), String> {
        check(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(VALUE_NAME)) })?;
        check(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(APPROVED_KEY), &HSTRING::from(VALUE_NAME)) })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    const ENTRY: &str = "app.amber.pc.desktop";

    fn config_dir(xdg: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
        if let Some(path) = xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
            return Ok(path);
        }
        home.map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| p.join(".config"))
            .ok_or_else(|| "Login startup requires an absolute XDG_CONFIG_HOME or HOME".into())
    }

    fn path() -> Result<PathBuf, String> {
        Ok(config_dir(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME"))?.join("autostart").join(ENTRY))
    }

    fn desktop_entry(exe: &Path) -> Result<String, String> {
        let exe = exe.to_str().ok_or("Login startup requires a UTF-8 executable path")?;
        if !Path::new(exe).is_absolute() || exe.chars().any(char::is_control) || exe.contains('=') {
            return Err("Login startup requires an absolute executable path without control characters or '='".into());
        }
        // Exec is not a shell command. Apply argument quoting, then Desktop Entry string
        // escaping. In particular, a literal backslash needs four backslashes on disk.
        let mut quoted = String::from("\"");
        for ch in exe.chars() {
            match ch {
                '\\' => quoted.push_str("\\\\\\\\"),
                '"' => quoted.push_str("\\\\\""),
                '$' => quoted.push_str("\\\\$"),
                '`' => quoted.push_str("\\\\`"),
                '%' => quoted.push_str("%%"),
                _ => quoted.push(ch),
            }
        }
        quoted.push('"');
        Ok(format!("[Desktop Entry]\nType=Application\nName=Amber\nExec={quoted} {}\nTerminal=false\n", amber_core::autostart::ARG))
    }

    fn disabled(content: &str) -> bool {
        let mut desktop_entry = false;
        for line in content.lines().map(str::trim) {
            if line.starts_with('[') {
                desktop_entry = line == "[Desktop Entry]";
            }
            if !desktop_entry {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if (key.trim() == "Hidden" && value.trim() == "true")
                    || (key.trim() == "X-GNOME-Autostart-enabled" && value.trim() == "false")
                {
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn disabled_by_desktop() -> Result<bool, String> {
        match std::fs::read_to_string(path()?) {
            Ok(content) => Ok(disabled(&content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("Cannot inspect login startup: {e}")),
        }
    }

    pub(super) fn set(on: bool) -> Result<(), String> {
        let path = path()?;
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            set_entry(&path, Some(&exe))
        } else {
            set_entry(&path, None)
        }
    }

    fn set_entry(path: &Path, executable: Option<&Path>) -> Result<(), String> {
        match executable {
            Some(exe) => crate::secrets::write_private(path, desktop_entry(exe)?.as_bytes()),
            None => crate::secrets::remove_private(path),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn xdg_paths_require_absolute_locations() {
            let os = |s: &str| Some(OsString::from(s));
            assert_eq!(config_dir(os("/custom config"), os("/home/user")).unwrap(), Path::new("/custom config"));
            assert_eq!(config_dir(os("relative"), os("/home/user")).unwrap(), Path::new("/home/user/.config"));
            assert_eq!(config_dir(os(""), os("/home/user")).unwrap(), Path::new("/home/user/.config"));
            assert!(config_dir(None, os("relative")).is_err());
            assert!(config_dir(None, None).is_err());
        }

        #[test]
        fn exec_is_one_quoted_executable_not_a_shell_command() {
            let entry = desktop_entry(Path::new("/opt/Amber App/amber")).unwrap();
            assert!(entry.contains("Exec=\"/opt/Amber App/amber\" --autostart\n"));
            let entry = desktop_entry(Path::new("/tmp/a\\b\"c$d`e%f;g' amber")).unwrap();
            assert!(entry.contains("Exec=\"/tmp/a\\\\\\\\b\\\\\"c\\\\$d\\\\`e%%f;g' amber\" --autostart\n"));
            assert!(!entry.contains("sh -c"));
        }

        #[test]
        fn desktop_entry_injection_is_rejected() {
            for path in ["relative/amber", "/tmp/amber\nHidden=false", "/tmp/a\rExec=bad", "/tmp/a\tbad", "/tmp/a=b/amber"] {
                assert!(desktop_entry(Path::new(path)).is_err());
            }
            use std::os::unix::ffi::OsStringExt;
            assert!(desktop_entry(Path::new(&OsString::from_vec(vec![b'/', 0xff]))).is_err());
        }

        #[test]
        fn desktop_disable_switch_is_respected_only_in_its_section() {
            assert!(disabled("[Desktop Entry]\nHidden=true\n"));
            assert!(disabled("[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"));
            assert!(!disabled("[Desktop Entry]\nHidden=false\n"));
            assert!(!disabled("[Other]\nHidden=true\n"));
            assert!(!disabled("[Desktop Entry]\n#Hidden=true\n"));
        }

        #[test]
        fn opt_in_entry_is_private_and_disabling_only_removes_the_entry() {
            use std::os::unix::fs::PermissionsExt;
            let mut random = [0u8; 8];
            getrandom::getrandom(&mut random).unwrap();
            let dir = std::env::temp_dir().join(format!("amber-autostart-test-{}", u64::from_le_bytes(random)));
            let path = dir.join("autostart").join(ENTRY);
            set_entry(&path, None).unwrap();
            assert!(!dir.exists());
            set_entry(&path, Some(Path::new("/opt/Amber App/amber"))).unwrap();
            assert!(std::fs::read_to_string(&path).unwrap().contains("--autostart"));
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
            set_entry(&path, None).unwrap();
            assert!(!path.exists());
            assert!(path.parent().unwrap().is_dir());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
