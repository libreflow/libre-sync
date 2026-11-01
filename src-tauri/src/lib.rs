mod repos;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            repos::list_repo_status,
            repos::add_repo,
            repos::remove_repo,
            repos::update_repo_path,
            repos::clone_repo,
            repos::pull_repo,
            repos::stash_pull_repo,
            repos::update_packages,
            repos::update_framework,
            repos::open_in_explorer,
            repos::gh_auth_status,
            repos::gh_connect_begin,
            repos::gh_connect_finish,
            repos::gh_connect_cancel,
            repos::github_device_code,
            repos::github_device_poll,
            repos::gh_login_with_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
