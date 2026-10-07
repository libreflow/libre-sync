mod repos;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            repos::list_repo_status,
            repos::clone_repo,
            repos::pull_repo,
            repos::stash_pull_repo,
            repos::update_packages,
            repos::update_framework,
            repos::open_in_explorer,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
