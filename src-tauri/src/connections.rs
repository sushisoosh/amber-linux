//! IPC for saved connections. Settings owns CRUD; these commands never infer a destination
//! from a stale ID and never borrow a built-in provider's existing credential.

use amber_core::connections::{self, Connection, Discovery, Resolved};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::Core;

pub fn key_for(core: &Core, target: &Resolved<'_>) -> Result<String, String> {
    let key = match target.connection {
        Some(connection) => core.connection_key(connection)?,
        None => core.key_for(target.provider.id, &target.base_url),
    }.unwrap_or_default();
    if target.provider.needs_key && key.is_empty() {
        return Err(if target.connection.is_some() {
            "No API key saved for this connection. Save its own key on the Connections page; existing provider keys are not shared.".into()
        } else { format!("No API key for {} yet - add it on the Credentials page.", target.provider.name) });
    }
    Ok(key)
}

fn request_key(core: &Core, connection: &Connection, key: Option<String>) -> Result<String, String> {
    // Also check identity when a draft key was supplied: a stale UI must not probe another
    // saved destination under the same connection ID. An explicit empty key is anonymous.
    let saved = core.connection_key(connection)?;
    let key = connections::validate_key(&key.or(saved).unwrap_or_default())?;
    let provider = amber_core::providers::by_id(&connection.provider).ok_or("Unknown connection adapter.")?;
    if provider.needs_key && key.is_empty() { return Err("Enter this connection's API key first.".into()); }
    Ok(key)
}

#[tauri::command]
pub fn has_connection_key(core: State<Core>, connection: Connection) -> Result<bool, String> {
    Ok(core.connection_key(&connection)?.is_some())
}

#[tauri::command]
pub async fn set_connection_key(app: AppHandle, connection: Connection, key: Option<String>) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = app.state::<Core>().set_connection_key(&connection, key);
        // Persistence errors may still leave a new session-only key. Never retain a catalog
        // fetched under the old credential, even when the key save reported an error.
        let _ = app.emit("connection-key-changed", &connection.id);
        result
    })
        .await.map_err(|_| "The credential worker failed.".to_string())?
}

#[tauri::command]
pub async fn test_connection(app: AppHandle, connection: Connection, key: Option<String>) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let key = request_key(&app.state::<Core>(), &connection, key)?;
        let c = connection.normalized()?;
        let provider = amber_core::providers::by_id(&c.provider).ok_or("Unknown connection adapter.")?;
        let started = std::time::Instant::now();
        amber_core::engine::check_provider(provider, &c.base_url, &key).map_err(|e| e.message)?;
        Ok(started.elapsed().as_millis() as u64)
    }).await.map_err(|_| "The connection test worker failed.".to_string())?
}

#[tauri::command]
pub async fn discover_connection_models(app: AppHandle, connection: Connection, key: Option<String>) -> Result<Discovery, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let key = request_key(&app.state::<Core>(), &connection, key)?;
        connections::discover(&connection, &key).map_err(|e| e.message)
    }).await.map_err(|_| "The model discovery worker failed.".to_string())?
}
