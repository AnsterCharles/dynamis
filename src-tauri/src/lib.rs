pub mod dap;
mod device;
pub mod samba;
mod stk500;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use device::{Device, Status};

#[tauri::command]
fn device_status(device: State<Arc<Device>>) -> Status {
    device.status()
}

#[tauri::command]
fn retry_device(device: State<Arc<Device>>) {
    device.retry();
}

fn recordings_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().document_dir().map_err(|e| e.to_string())?.join("Dynamis");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Writes a CSV into Documents/Dynamis and returns the full path.
#[tauri::command]
fn save_csv(app: AppHandle, file_name: String, contents: String) -> Result<String, String> {
    let safe: String = file_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if safe.is_empty() {
        return Err("Invalid file name".into());
    }
    let path = recordings_dir(&app)?.join(format!("{safe}.csv"));
    std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
fn reveal_file(app: AppHandle, path: String) -> Result<(), String> {
    // Only reveal files inside our own recordings folder.
    let path = PathBuf::from(path);
    if !path.starts_with(recordings_dir(&app)?) {
        return Err("Not a recording file".into());
    }
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let device = Device::new();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(device.clone())
        .setup(move |app| {
            device.start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![device_status, retry_device, save_csv, reveal_file])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
