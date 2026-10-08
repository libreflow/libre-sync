//! Core logic for LibreSync: clone-or-update every repo on the
//! `defaultOwner` GitHub account from one place, on this machine or a
//! fresh one.
//!
//! Ported from the original PowerShell hub (`W:\github-hub\hub.ps1`,
//! see the `github-hub-conventions` skill for the history/decisions
//! behind the default paths below) -- same git plumbing, same state
//! model, now with a GUI instead of a console menu.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Hard ceiling on any single npm/cargo subprocess. Without this, a
/// stalled network call (observed in practice: a `cargo update` index
/// fetch that hung with the subprocess sitting at 0% CPU) blocks the
/// Tauri command forever and the UI is stuck on "En cours..." with no
/// way out short of restarting the app.
const PACKAGE_CMD_TIMEOUT: Duration = Duration::from_secs(150);

const GIT_CMD_TIMEOUT: Duration = Duration::from_secs(300);

const CLONE_CMD_TIMEOUT: Duration = Duration::from_secs(1800);

fn spawn_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(cmd.output());
    });
    rx.recv_timeout(timeout).ok()?.ok()
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runs `git -C <path> <args>`
fn git(path: &str, args: &[&str]) -> Option<Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    spawn_with_timeout(cmd, GIT_CMD_TIMEOUT)
}

/// Same as `git`, but for commands run from `path` without
/// necessarily being inside a repo yet (clone's destination doesn't
/// exist until the command succeeds, so `-C` would fail before trying).
fn run(cmd_name: &str, args: &[&str]) -> Option<Output> {
    let mut cmd = Command::new(cmd_name);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    spawn_with_timeout(cmd, GIT_CMD_TIMEOUT)
}

/// Clone variant of `run`: uses a longer timeout -- cloning a large
/// repository over a slow network legitimately takes many minutes; the
/// shorter `GIT_CMD_TIMEOUT` would kill healthy clones.
fn run_clone(cmd_name: &str, args: &[&str]) -> Option<Output> {
    let mut cmd = Command::new(cmd_name);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    spawn_with_timeout(cmd, CLONE_CMD_TIMEOUT)
}

/// Same as `run`, but in a specific working directory -- for commands
/// scoped to a subfolder (e.g. a Tauri app's `src-tauri/`), independent
/// of the git-specific `-C` flag `git()` uses. Runs the subprocess on a
/// helper thread and gives up after `PACKAGE_CMD_TIMEOUT` so a stalled
/// child (e.g. a hung network fetch) can never freeze the caller --
/// the orphaned process/thread may linger, but this function always
/// returns.
fn run_in(cmd_name: &str, args: &[&str], cwd: &str) -> Option<Output> {
    let mut cmd = Command::new(cmd_name);
    cmd.args(args).current_dir(cwd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    spawn_with_timeout(cmd, PACKAGE_CMD_TIMEOUT)
}

/// npm on Windows ships as `npm.cmd` (a batch wrapper), not a real
/// `.exe` -- `Command::new("npm")` fails to find/execute it directly
/// (CreateProcess doesn't run batch files the way a shell does). Always
/// go through `cmd /c` for npm specifically; `cargo`/`git`/`gh` are real
/// executables and don't need this.
fn run_npm(args: &[&str], cwd: &str) -> Option<Output> {
    #[cfg(windows)]
    {
        let mut full_args: Vec<&str> = vec!["/c", "npm"];
        full_args.extend_from_slice(args);
        run_in("cmd", &full_args, cwd)
    }
    #[cfg(not(windows))]
    {
        run_in("npm", args, cwd)
    }
}

/// npm/cargo render progress bars by repeatedly overwriting the current
/// line with `\r` when they believe they have a terminal to animate --
/// captured non-interactively, those `\r`-separated frames all land in
/// the string as-is instead of actually overwriting anything, so a
/// naive display shows every intermediate frame stacked up (seen in
/// practice: a wall of "Fetch [=>....] 1% / Fetch [==>...] 4% / ..."
/// for what was already a finished, successful command). Keep only the
/// text after the last `\r` on each line -- i.e. whatever the terminal
/// would actually be showing once the overwrites settle.
fn clean_progress_output(raw: &str) -> String {
    raw.split('\n')
        .map(|line| line.rsplit('\r').next().unwrap_or(line).trim_end())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// stdout of a successful git call, trimmed; `None` on any failure or
/// empty output (empty porcelain status, no upstream configured, etc.)
fn git_capture(path: &str, args: &[&str]) -> Option<String> {
    let out = git(path, args)?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    pub owner: String,
    pub path: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubConfig {
    pub default_base_dir: String,
    pub default_owner: String,
    pub repos: Vec<RepoConfig>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    pub name: String,
    pub owner: String,
    pub path: String,
    pub state: String,
    pub detail: String,
    pub dirty: bool,
}

/// First-run seed, carried over verbatim from the validated mapping in
/// `W:\github-hub\repos.json` (every path there was confirmed against a
/// real local clone or a deliberate "not cloned here yet" -- see
/// `github-hub-conventions` skill for the reasoning behind each one,
/// especially `unbreakable-repo` and `local-llm-client-fresh`).
fn default_config() -> HubConfig {
    let repos: [(&str, &str); 12] = [
        ("libre-media-player", "W:/libre-media-player"),
        ("libreflow", "W:/libreflow"),
        ("ShutdownPro", "W:/ShutdownPro"),
        ("BetterUnistaller", "W:/BetterUnistaller"),
        ("local-llm-client", "W:/local-llm-client"),
        ("BudgetManager", "W:/BudgetManager"),
        ("Libreflow-v2", "W:/Libreflow-v2"),
        ("unbreakable", "W:/unbreakable-repo"),
        ("fasttt", "W:/fasttt"),
        ("CV_Prime", "W:/CV_Prime"),
        ("shiptrack", "W:/shiptrack"),
        ("PrinterRemover", "W:/PrinterRemover"),
    ];
    HubConfig {
        default_base_dir: "W:/".to_string(),
        default_owner: "libreflow".to_string(),
        repos: repos
            .into_iter()
            .map(|(name, path)| RepoConfig {
                name: name.to_string(),
                owner: "libreflow".to_string(),
                path: path.to_string(),
            })
            .collect(),
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Dossier de config introuvable: {e}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Creation du dossier de config impossible: {e}"))?;
    Ok(dir.join("repos.json"))
}

pub fn load_config(app: &AppHandle) -> Result<HubConfig, String> {
    let path = config_path(app)?;
    if !path.exists() {
        let cfg = default_config();
        save_config(app, &cfg)?;
        return Ok(cfg);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("Lecture de la config impossible: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("Config invalide: {e}"))
}

pub fn save_config(app: &AppHandle, cfg: &HubConfig) -> Result<(), String> {
    let path = config_path(app)?;
    let json = serde_json::to_string_pretty(cfg)
        .map_err(|e| format!("Serialisation de la config impossible: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Ecriture de la config impossible: {e}"))
}

/// Pure decision logic, kept separate from the git plumbing so it can be
/// unit tested with fixed inputs instead of real repos (see tests below).
pub fn classify_state(
    local: Option<&str>,
    upstream: Option<&str>,
    ahead: u32,
    behind: u32,
) -> (String, String) {
    match (local, upstream) {
        (Some(l), Some(u)) if l == u => ("a-jour".to_string(), String::new()),
        (Some(_), Some(_)) => {
            if ahead > 0 {
                (
                    "divergent".to_string(),
                    format!("{ahead} commit(s) local non pousse(s), {behind} en retard"),
                )
            } else {
                (
                    "en-retard".to_string(),
                    format!("{behind} commit(s) a recuperer"),
                )
            }
        }
        _ => (
            "erreur".to_string(),
            "pas de branche amont configuree".to_string(),
        ),
    }
}

fn check_repo(repo: &RepoConfig) -> RepoStatus {
    let git_dir = Path::new(&repo.path).join(".git");
    if !git_dir.exists() {
        return RepoStatus {
            name: repo.name.clone(),
            owner: repo.owner.clone(),
            path: repo.path.clone(),
            state: "non-clone".to_string(),
            detail: format!("sera clone dans {}", repo.path),
            dirty: false,
        };
    }

    git(&repo.path, &["fetch", "--quiet"]);
    let local = git_capture(&repo.path, &["rev-parse", "HEAD"]);
    let detached = git_capture(&repo.path, &["symbolic-ref", "-q", "HEAD"]).is_none();
    // Surfaced to the UI so it can offer stash_pull_repo -- the app's
    // own package/framework updates leave uncommitted lockfile changes
    // that block pull_repo by design.
    let dirty = git(
        &repo.path,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
    .unwrap_or(false);
    let upstream = if detached {
        None
    } else {
        git_capture(&repo.path, &["rev-parse", "@{u}"])
    };

    let (ahead, behind) = match (&local, &upstream) {
        (Some(l), Some(u)) if l != u => {
            let a = git_capture(&repo.path, &["rev-list", "--count", &format!("{u}..{l}")])
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let b = git_capture(&repo.path, &["rev-list", "--count", &format!("{l}..{u}")])
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            (a, b)
        }
        _ => (0, 0),
    };

    let (state, detail) = classify_state(local.as_deref(), upstream.as_deref(), ahead, behind);
    RepoStatus {
        name: repo.name.clone(),
        owner: repo.owner.clone(),
        path: repo.path.clone(),
        state,
        detail,
        dirty,
    }
}

fn gh_available() -> bool {
    run("gh", &["--version"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Appends any repo that exists on GitHub but isn't in the config yet,
/// with a default path under `defaultBaseDir`. Never touches or
/// relocates an existing entry's path.
fn sync_from_github(cfg: &mut HubConfig) {
    if !gh_available() {
        return;
    }
    let Some(out) = run(
        "gh",
        &[
            "repo",
            "list",
            &cfg.default_owner,
            "--limit",
            "1000",
            "--json",
            "name",
            "owner",
        ],
    ) else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Ok(text) = String::from_utf8(out.stdout) else {
        return;
    };
    let Ok(remote) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return;
    };
    for item in remote {
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let owner = item
            .get("owner")
            .and_then(|o| o.get("login"))
            .and_then(|v| v.as_str())
            .unwrap_or(&cfg.default_owner);
        // Compare on the (name, owner) pair, not the name alone: two
        // different accounts can own same-named repos, and a name-only
        // match would silently skip a genuinely new (or forked) repo.
        if cfg.repos.iter().any(|r| r.name == name && r.owner == owner) {
            continue;
        }
        cfg.repos.push(RepoConfig {
            name: name.to_string(),
            owner: owner.to_string(),
            path: format!("{}{}", cfg.default_base_dir, name),
        });
    }
}

#[tauri::command]
pub fn list_repo_status(app: AppHandle) -> Result<Vec<RepoStatus>, String> {
    let mut cfg = load_config(&app)?;
    let before = cfg.repos.len();
    sync_from_github(&mut cfg);
    if cfg.repos.len() != before {
        save_config(&app, &cfg)?;
    }

    // Each check is a handful of short-lived git subprocesses plus one
    // network fetch -- worth parallelizing once there's a dozen+ repos.
    let handles: Vec<_> = cfg
        .repos
        .iter()
        .cloned()
        .map(|repo| std::thread::spawn(move || check_repo(&repo)))
        .collect();

    let mut statuses: Vec<RepoStatus> = Vec::with_capacity(handles.len());
    for (handle, repo) in handles.into_iter().zip(cfg.repos.iter()) {
        match handle.join() {
            Ok(status) => statuses.push(status),
            Err(_) => statuses.push(RepoStatus {
                name: repo.name.clone(),
                owner: repo.owner.clone(),
                path: repo.path.clone(),
                state: "erreur".to_string(),
                detail: "verification interrompue (thread panique)".to_string(),
                dirty: false,
            }),
        }
    }
    Ok(statuses)
}

/// Add a repo to the config. Rejects an entry that already exists --
/// same (name, owner) pair -- so a manual add can never create the
/// duplicate sync_from_github's dedup guards against.
#[tauri::command]
pub fn add_repo(
    app: AppHandle,
    name: String,
    owner: String,
    path: String,
) -> Result<String, String> {
    let name = name.trim();
    let owner = owner.trim();
    let path = path.trim();
    if name.is_empty() || owner.is_empty() || path.is_empty() {
        return Err("Nom, owner et chemin sont obligatoires.".to_string());
    }
    let mut cfg = load_config(&app)?;
    if cfg.repos.iter().any(|r| r.name == name && r.owner == owner) {
        return Err(format!("{owner}/{name} est deja dans la configuration."));
    }
    cfg.repos.push(RepoConfig {
        name: name.to_string(),
        owner: owner.to_string(),
        path: path.to_string(),
    });
    save_config(&app, &cfg)?;
    Ok(format!("{owner}/{name} ajoute ({path})."))
}

/// Remove a repo from the config. Never touches the clone on disk --
/// removing an entry must not look like it deletes the working copy.
#[tauri::command]
pub fn remove_repo(app: AppHandle, name: String, owner: String) -> Result<String, String> {
    let mut cfg = load_config(&app)?;
    let before = cfg.repos.len();
    cfg.repos.retain(|r| !(r.name == name && r.owner == owner));
    if cfg.repos.len() == before {
        return Err(format!("{owner}/{name} n'est pas dans la configuration."));
    }
    save_config(&app, &cfg)?;
    Ok(format!(
        "{owner}/{name} retire de la configuration (le dossier local n'a pas ete touche)."
    ))
}

/// Edit a repo's local path -- the "I moved my clone" case. Rejects a
/// path that would collide with another entry's path.
#[tauri::command]
pub fn update_repo_path(
    app: AppHandle,
    name: String,
    owner: String,
    path: String,
) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("Le chemin est obligatoire.".to_string());
    }
    let mut cfg = load_config(&app)?;
    if cfg
        .repos
        .iter()
        .any(|r| r.name != name && r.owner == owner && r.path == path)
    {
        return Err(format!(
            "Le chemin {path} est deja utilise par un autre depot."
        ));
    }
    let repo = cfg
        .repos
        .iter_mut()
        .find(|r| r.name == name && r.owner == owner)
        .ok_or_else(|| format!("{owner}/{name} n'est pas dans la configuration."))?;
    let old = repo.path.clone();
    repo.path = path.to_string();
    save_config(&app, &cfg)?;
    Ok(format!(
        "Chemin de {owner}/{name} mis a jour : {old} -> {path}"
    ))
}

#[tauri::command]
pub fn clone_repo(app: AppHandle, name: String) -> Result<String, String> {
    let cfg = load_config(&app)?;
    let repo = cfg
        .repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| "Depot inconnu.".to_string())?;

    if Path::new(&repo.path).join(".git").exists() {
        return Err("Ce depot est deja clone.".to_string());
    }

    if let Some(parent) = Path::new(&repo.path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Creation du dossier parent impossible: {e}"))?;
    }

    let output = if gh_available() {
        run_clone(
            "gh",
            &[
                "repo",
                "clone",
                &format!("{}/{}", repo.owner, repo.name),
                &repo.path,
            ],
        )
    } else {
        let url = format!("https://github.com/{}/{}.git", repo.owner, repo.name);
        run_clone("git", &["clone", &url, &repo.path])
    }
    .ok_or_else(|| "Impossible de lancer git/gh, ou delai depasse.".to_string())?;

    if output.status.success() {
        Ok(format!("clone avec succes dans {}", repo.path))
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[tauri::command]
pub fn pull_repo(app: AppHandle, name: String) -> Result<String, String> {
    let cfg = load_config(&app)?;
    let repo = cfg
        .repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| "Depot inconnu.".to_string())?;

    // Check the worktree through git itself rather than piggybacking on
    // git_capture: git_capture returns None for both "clean tree" and
    // "git status failed" (e.g. a corrupted repo), and a failed status
    // check must not be mistaken for a clean, pullable tree.
    let status_out = git(
        &repo.path,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .ok_or_else(|| "Impossible de lancer git.".to_string())?;
    if !status_out.status.success() {
        return Err(format!(
            "git status a echoue -- mise a jour annulee:\n{}",
            String::from_utf8_lossy(&status_out.stderr).trim()
        ));
    }
    let dirty = String::from_utf8_lossy(&status_out.stdout)
        .trim()
        .to_string();
    if !dirty.is_empty() {
        return Err(format!(
            "Modifications locales non enregistrees -- mise a jour annulee:\n{dirty}"
        ));
    }

    let output = git(&repo.path, &["pull", "--ff-only"])
        .ok_or_else(|| "Impossible de lancer git.".to_string())?;

    if output.status.success() {
        let msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if msg.is_empty() {
            "deja a jour".to_string()
        } else {
            msg
        })
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Escape hatch for the UX deadlock `pull_repo`'s dirty-tree guard
/// creates: the app's own package/framework updates deliberately leave
/// lockfile changes uncommitted, which then block the pull with
/// "Modifications locales non enregistrees". Stashes the tracked
/// changes, pulls --ff-only, then pops the stash so nothing is lost.
/// If the pull fails, the stash is restored first; if the pop itself
/// conflicts, the stash entry is kept -- the user resolves by hand and
/// no work is silently dropped.
#[tauri::command]
pub fn stash_pull_repo(app: AppHandle, name: String) -> Result<String, String> {
    let cfg = load_config(&app)?;
    let repo = cfg
        .repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| "Depot inconnu.".to_string())?;

    if !Path::new(&repo.path).join(".git").exists() {
        return Err("Ce depot n'est pas encore clone.".to_string());
    }

    let stash_out = git(&repo.path, &["stash", "push", "-m", "libre-sync pull"])
        .ok_or_else(|| "Impossible de lancer git.".to_string())?;
    if !stash_out.status.success() {
        return Err(format!(
            "git stash a echoue -- mise a jour annulee:\n{}",
            String::from_utf8_lossy(&stash_out.stderr).trim()
        ));
    }
    // On a clean tree `git stash push` exits 0 but creates no stash
    // entry ("No local changes to save") -- popping then would fail.
    let stash_created = !String::from_utf8_lossy(&stash_out.stdout)
        .trim()
        .contains("No local changes to save");

    let pull_out = git(&repo.path, &["pull", "--ff-only"]);
    let pull_msg = match pull_out {
        Some(out) if out.status.success() => {
            let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if msg.is_empty() {
                "deja a jour".to_string()
            } else {
                msg
            }
        }
        Some(out) => {
            let _ = git(&repo.path, &["stash", "pop"]);
            return Err(format!(
                "git pull a echoue (stash restaure) -- {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        None => {
            let _ = git(&repo.path, &["stash", "pop"]);
            return Err("Impossible de lancer git.".to_string());
        }
    };

    if stash_created {
        let pop_out = git(&repo.path, &["stash", "pop"])
            .ok_or_else(|| "Impossible de lancer git.".to_string())?;
        if !pop_out.status.success() {
            return Err(format!(
                "Mise a jour OK ({pull_msg}) mais restauration du stash en conflit -- tes modifications sont conservees dans le stash (git stash list / git stash pop), a resoudre manuellement:\n{}",
                String::from_utf8_lossy(&pop_out.stderr).trim()
            ));
        }
    }

    Ok(if stash_created {
        format!("{pull_msg}\n(modifications locales mises de cote puis restaurees)")
    } else {
        pull_msg
    })
}

/// "Douce" package update: `npm update` for a `package.json` at the repo
/// root, `cargo update` for a `Cargo.toml` at the root and/or
/// `src-tauri/` (Tauri's usual layout) -- both only ever move within the
/// version ranges already declared (package.json's ^/~, Cargo.toml's
/// default caret requirement), never a breaking/major bump. Leaves the
/// resulting lockfile changes uncommitted in the working tree, same as
/// any other local edit -- this command never commits or pushes.
#[tauri::command]
pub fn update_packages(app: AppHandle, name: String) -> Result<String, String> {
    let cfg = load_config(&app)?;
    let repo = cfg
        .repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| "Depot inconnu.".to_string())?;

    if !Path::new(&repo.path).join(".git").exists() {
        return Err("Ce depot n'est pas encore clone.".to_string());
    }

    let mut messages: Vec<String> = Vec::new();
    let mut found_a_manifest = false;

    if Path::new(&repo.path).join("package.json").exists() {
        found_a_manifest = true;
        match run_npm(&["update"], &repo.path) {
            None => messages.push("npm : introuvable, ou delai depasse (>2min30).".to_string()),
            Some(out) => {
                let stdout = clean_progress_output(&String::from_utf8_lossy(&out.stdout));
                let stderr = clean_progress_output(&String::from_utf8_lossy(&out.stderr));
                if out.status.success() {
                    let combined = [stdout, stderr].join("\n").trim().to_string();
                    messages.push(if combined.is_empty() {
                        "npm : deja a jour.".to_string()
                    } else {
                        format!("npm update:\n{combined}")
                    });
                } else {
                    messages.push(format!("npm : echec -- {stderr}"));
                }
            }
        }
    }

    for candidate in ["Cargo.toml", "src-tauri/Cargo.toml"] {
        let cargo_toml = Path::new(&repo.path).join(candidate);
        if !cargo_toml.exists() {
            continue;
        }
        found_a_manifest = true;
        let cargo_dir = cargo_toml
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(&repo.path);
        match run_in("cargo", &["update"], cargo_dir) {
            None => messages.push("cargo : introuvable, ou delai depasse (>2min30).".to_string()),
            Some(out) => {
                // cargo update reports its work on stderr, stdout is
                // normally empty -- this is cargo's own convention, not
                // a sign of failure.
                let stderr = clean_progress_output(&String::from_utf8_lossy(&out.stderr));
                if out.status.success() {
                    messages.push(if stderr.is_empty() {
                        format!("cargo ({candidate}) : deja a jour.")
                    } else {
                        format!("cargo update ({candidate}):\n{stderr}")
                    });
                } else {
                    messages.push(format!("cargo ({candidate}) : echec -- {stderr}"));
                }
            }
        }
    }

    if !found_a_manifest {
        return Err("Aucun package.json ni Cargo.toml trouve dans ce depot.".to_string());
    }

    Ok(messages.join("\n\n"))
}

/// Which lines of a Cargo.toml's dependency sections are Tauri's own
/// crates (`tauri`, `tauri-build`, any `tauri-plugin-*`) -- `true`
/// alongside a name means it lives in `[build-dependencies]` (so
/// `cargo add` needs `--build` to update the right section instead of
/// adding a stray duplicate under `[dependencies]`). Line-based on
/// purpose: these manifests are always the plain Tauri-template shape,
/// and pulling in a full TOML-parsing crate for this one scan isn't
/// worth the extra dependency.
fn find_tauri_crate_deps(cargo_toml: &str) -> Vec<(String, bool)> {
    // false = [dependencies], true = [build-dependencies], None = any
    // other section ([package], [features], [workspace.dependencies],
    // [target.'cfg(...)'.dependencies], ...) -- a `tauri` key there is
    // either metadata or a shared workspace definition, never a plain
    // dependency this manifest's `cargo add` should touch.
    let mut section: Option<bool> = None;
    let mut found = Vec::new();
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = match trimmed {
                "[dependencies]" => Some(false),
                "[build-dependencies]" => Some(true),
                _ => None,
            };
            continue;
        }
        if section.is_none() {
            continue;
        }
        if !trimmed.starts_with("tauri") {
            continue;
        }
        if let Some(key) = trimmed.split('=').next() {
            let key = key.trim();
            if key == "tauri" || key == "tauri-build" || key.starts_with("tauri-plugin-") {
                found.push((key.to_string(), section == Some(true)));
            }
        }
    }
    found
}

/// Bump Vite and Tauri (both the npm and the Rust/Cargo side) to their
/// latest releases -- deliberately crosses major versions if needed,
/// unlike `update_packages`'s soft mode. Scoped to exactly these two
/// frameworks, nothing else: Tauri's own compatibility promise is that
/// upgrades within the same major never break your code (see
/// updating-dependencies in the Tauri docs), and every repo here is
/// already on Tauri 2, so this is a deliberate "stay current on the
/// framework" action rather than a blanket "update everything"
/// upgrade. Same uncommitted-lockfile-changes philosophy as
/// `update_packages`: this never commits or pushes, the user reviews
/// and tests before committing.
#[tauri::command]
pub fn update_framework(app: AppHandle, name: String) -> Result<String, String> {
    let cfg = load_config(&app)?;
    let repo = cfg
        .repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| "Depot inconnu.".to_string())?;

    if !Path::new(&repo.path).join(".git").exists() {
        return Err("Ce depot n'est pas encore clone.".to_string());
    }

    let mut messages: Vec<String> = Vec::new();
    let mut found_anything = false;

    // --- Vite + the Tauri JS packages (package.json, repo root) ---
    let pkg_json_path = Path::new(&repo.path).join("package.json");
    if let Ok(pkg_json) = std::fs::read_to_string(&pkg_json_path) {
        let candidates = [
            "vite",
            "@vitejs/plugin-react",
            "@tauri-apps/cli",
            "@tauri-apps/api",
        ];
        let npm_targets: Vec<&str> = candidates
            .iter()
            .filter(|pkg| pkg_json.contains(&format!("\"{pkg}\"")))
            .copied()
            .collect();

        if !npm_targets.is_empty() {
            found_anything = true;
            let bumped: Vec<String> = npm_targets.iter().map(|p| format!("{p}@latest")).collect();
            let mut args: Vec<&str> = vec!["install"];
            args.extend(bumped.iter().map(|s| s.as_str()));
            match run_npm(&args, &repo.path) {
                None => messages.push("npm : introuvable, ou delai depasse (>2min30).".to_string()),
                Some(out) => {
                    let stdout = clean_progress_output(&String::from_utf8_lossy(&out.stdout));
                    let stderr = clean_progress_output(&String::from_utf8_lossy(&out.stderr));
                    if out.status.success() {
                        let combined = [stdout, stderr].join("\n").trim().to_string();
                        messages.push(format!(
                            "npm ({}):\n{}",
                            npm_targets.join(", "),
                            if combined.is_empty() {
                                "deja a jour.".to_string()
                            } else {
                                combined
                            }
                        ));
                        // Vite major bumps can break the build (plugin
                        // API / config schema changes) -- verify rather
                        // than leave that discovery for later.
                        match run_npm(&["run", "build"], &repo.path) {
                            None => messages
                                .push("verification (npm run build) : delai depasse.".to_string()),
                            Some(check) if check.status.success() => {
                                messages.push("verification : npm run build OK.".to_string())
                            }
                            Some(check) => {
                                let err =
                                    clean_progress_output(&String::from_utf8_lossy(&check.stderr));
                                messages.push(format!(
                                    "/!\\ verification : npm run build ECHOUE apres la mise a jour -- le projet ne compile plus.\n{err}\nRevert possible : git checkout -- package.json package-lock.json"
                                ));
                            }
                        }
                    } else {
                        messages.push(format!("npm : echec -- {stderr}"));
                    }
                }
            }
        }
    }

    // --- Tauri's Rust crates (Cargo.toml, root and/or src-tauri/) ---
    for candidate in ["Cargo.toml", "src-tauri/Cargo.toml"] {
        let cargo_toml_path = Path::new(&repo.path).join(candidate);
        let Ok(content) = std::fs::read_to_string(&cargo_toml_path) else {
            continue;
        };
        let cargo_dir = cargo_toml_path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(&repo.path);

        let targets = find_tauri_crate_deps(&content);
        if targets.is_empty() {
            continue;
        }
        found_anything = true;

        for (dep_name, is_build) in &targets {
            let mut args: Vec<&str> = vec!["add", dep_name.as_str()];
            if *is_build {
                args.push("--build");
            }
            match run_in("cargo", &args, cargo_dir) {
                None => messages.push(format!(
                    "cargo add {dep_name} : introuvable, ou delai depasse (>2min30)."
                )),
                Some(out) => {
                    let stderr = clean_progress_output(&String::from_utf8_lossy(&out.stderr));
                    if out.status.success() {
                        messages.push(format!(
                            "cargo ({candidate}) {dep_name}:\n{}",
                            if stderr.is_empty() {
                                "deja a jour.".to_string()
                            } else {
                                stderr
                            }
                        ));
                    } else {
                        messages.push(format!(
                            "cargo ({candidate}) {dep_name} : echec -- {stderr}"
                        ));
                    }
                }
            }
        }

        // Same reasoning as the npm build check above: a transitive
        // crate (seen in practice: `windows` jumping 0.61->0.62 as a
        // side effect of bumping tauri) can break compilation even
        // though Tauri's own same-major promise holds. One check for
        // the whole dependency set, not per-crate.
        match run_in("cargo", &["check"], cargo_dir) {
            None => messages.push("verification (cargo check) : delai depasse.".to_string()),
            Some(check) if check.status.success() => {
                messages.push("verification : cargo check OK.".to_string())
            }
            Some(check) => {
                let err = clean_progress_output(&String::from_utf8_lossy(&check.stderr));
                let lockfile = candidate.replace("Cargo.toml", "Cargo.lock");
                messages.push(format!(
                    "/!\\ verification : cargo check ECHOUE apres la mise a jour -- le projet ne compile plus.\n{err}\nRevert possible : git checkout -- {candidate} {lockfile}"
                ));
            }
        }
    }

    if !found_anything {
        return Err("Ni Vite ni Tauri detectes dans ce depot.".to_string());
    }

    Ok(messages.join("\n\n"))
}

#[tauri::command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Le chemin n'existe pas : {path}"));
    }
    #[cfg(windows)]
    {
        Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Impossible d'ouvrir l'explorateur: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Impossible d'ouvrir le Finder: {e}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Impossible d'ouvrir le gestionnaire de fichiers: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_progress_output_keeps_only_the_last_carriage_return_frame() {
        let raw = "Updating crates.io index\nFetch [=>....] 1%\rFetch [===>.] 40%\rFetch [=====] 100%\nLocking 2 packages\n";
        assert_eq!(
            clean_progress_output(raw),
            "Updating crates.io index\nFetch [=====] 100%\nLocking 2 packages"
        );
    }

    #[test]
    fn clean_progress_output_drops_blank_lines() {
        assert_eq!(clean_progress_output("a\n\n\nb\n"), "a\nb");
        assert_eq!(clean_progress_output(""), "");
    }

    #[test]
    fn find_tauri_crate_deps_finds_tauri_and_build_dep_and_plugins() {
        let cargo_toml = r#"
[package]
name = "tauri-app"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-opener = "2"
tauri-plugin-autostart = "2"
serde = { version = "1", features = ["derive"] }
"#;
        let found = find_tauri_crate_deps(cargo_toml);
        assert_eq!(
            found,
            vec![
                ("tauri-build".to_string(), true),
                ("tauri".to_string(), false),
                ("tauri-plugin-opener".to_string(), false),
                ("tauri-plugin-autostart".to_string(), false),
            ]
        );
    }

    #[test]
    fn find_tauri_crate_deps_ignores_unrelated_crates_and_comments() {
        let cargo_toml = "[dependencies]\n# tauri-like-but-not = \"1\"\nserde = \"1\"\ntauric-other-crate = \"1\"\n";
        assert_eq!(
            find_tauri_crate_deps(cargo_toml),
            Vec::<(String, bool)>::new()
        );
    }

    #[test]
    fn up_to_date_when_local_matches_upstream() {
        let (state, detail) = classify_state(Some("abc"), Some("abc"), 0, 0);
        assert_eq!(state, "a-jour");
        assert_eq!(detail, "");
    }

    #[test]
    fn behind_when_upstream_has_new_commits() {
        let (state, detail) = classify_state(Some("abc"), Some("def"), 0, 3);
        assert_eq!(state, "en-retard");
        assert_eq!(detail, "3 commit(s) a recuperer");
    }

    #[test]
    fn diverged_when_local_has_unpushed_commits() {
        let (state, detail) = classify_state(Some("abc"), Some("def"), 2, 1);
        assert_eq!(state, "divergent");
        assert_eq!(detail, "2 commit(s) local non pousse(s), 1 en retard");
    }

    #[test]
    fn error_when_no_upstream_configured() {
        let (state, detail) = classify_state(Some("abc"), None, 0, 0);
        assert_eq!(state, "erreur");
        assert_eq!(detail, "pas de branche amont configuree");
    }

    #[test]
    fn default_config_has_all_twelve_known_repos() {
        let cfg = default_config();
        assert_eq!(cfg.repos.len(), 12);
        assert!(cfg.repos.iter().any(|r| r.name == "libre-media-player"));
        // The disambiguated paths must survive verbatim -- regressing
        // either of these silently re-creates a real collision/data-loss
        // bug that was already found and fixed once (see
        // github-hub-conventions skill).
        assert!(cfg
            .repos
            .iter()
            .any(|r| r.name == "unbreakable" && r.path == "W:/unbreakable-repo"));
        assert!(cfg
            .repos
            .iter()
            .any(|r| r.name == "local-llm-client" && r.path == "W:/local-llm-client"));
    }
}
