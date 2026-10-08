import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type RepoState = "non-clone" | "a-jour" | "en-retard" | "divergent" | "erreur";

interface RepoStatus {
  name: string;
  owner: string;
  path: string;
  state: RepoState;
  detail: string;
  dirty: boolean;
}

const STATE_LABEL: Record<RepoState, string> = {
  "a-jour": "A jour",
  "en-retard": "Mise a jour disponible",
  "non-clone": "Non clone",
  divergent: "Commits non pousses",
  erreur: "A verifier",
};

const STATE_DOT: Record<RepoState, string> = {
  "a-jour": "dot dot-ok",
  "en-retard": "dot dot-update",
  "non-clone": "dot dot-missing",
  divergent: "dot dot-warn",
  erreur: "dot dot-warn",
};

const ACTIONABLE: RepoState[] = ["non-clone", "en-retard"];

const LOG_MAX_ENTRIES = 200;

interface RepoForm {
  name: string;
  owner: string;
  path: string;
}

function actionLabel(state: RepoState): string {
  switch (state) {
    case "non-clone":
      return "Cloner";
    default:
      return "Mettre a jour";
  }
}

function App() {
  const [repos, setRepos] = useState<RepoStatus[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [activeName, setActiveName] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [inFlight, setInFlight] = useState<string | null>(null);
  const [log, setLog] = useState<string[]>([]);
  // Synchronous guard: `busy` (React state) only reflects on the next
  // render, so a double-click before that re-render would start a
  // second concurrent clone/pull loop. This ref flips in the same tick.
  const busyRef = useRef(false);
  // Cancellation is checked between repos of a batch: the in-flight
  // subprocess (a clone can run for minutes) is left to finish, then
  // the loop stops.
  const cancelRef = useRef(false);
  const [showAddForm, setShowAddForm] = useState(false);
  const [newRepo, setNewRepo] = useState<RepoForm>({ name: "", owner: "", path: "" });
  const [editingPath, setEditingPath] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);

  const appendLog = useCallback((entry: string) => {
    const time = new Date().toLocaleTimeString();
    setLog((l) => [`${time} -- ${entry}`, ...l.slice(0, LOG_MAX_ENTRIES - 1)]);
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const result = await invoke<RepoStatus[]>("list_repo_status");
      result.sort((a, b) => a.name.localeCompare(b.name));
      setRepos(result);
    } catch (e) {
      setLoadError(String(e));
      appendLog(`Erreur de chargement : ${String(e)}`);
    } finally {
      setLoading(false);
    }
  }, [appendLog]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const filteredRepos = useMemo(
    () => repos.filter((r) => r.name.toLowerCase().includes(filter.toLowerCase())),
    [repos, filter],
  );

  const actionableRepos = repos.filter((r) => ACTIONABLE.includes(r.state));
  // Select-all operates on what the filter actually shows: selecting
  // rows the user cannot see (count larger than the visible list) is
  // more confusing than helpful.
  const visibleRepos = filteredRepos;
  const allVisibleSelected =
    visibleRepos.length > 0 && visibleRepos.every((r) => selected.has(r.name));

  const toggle = (name: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const toggleAll = () => {
    setSelected(allVisibleSelected ? new Set() : new Set(visibleRepos.map((r) => r.name)));
  };

  const runOn = async (targets: RepoStatus[]) => {
    if (targets.length === 0 || busyRef.current) return;
    busyRef.current = true;
    cancelRef.current = false;
    setBusy(true);
    let ok = 0;
    let failed = 0;
    let skipped = 0;
    let cancelled = false;
    for (const repo of targets) {
      if (cancelRef.current) {
        cancelled = true;
        break;
      }
      setInFlight(repo.name);
      // Non-actionable states in a batch (user can now select
      // up-to-date or divergent repos): skip clone/pull attempts that
      // would fail or do nothing, with an explicit log line instead.
      if (repo.state === "a-jour" && !repo.dirty) {
        appendLog(`${repo.name} -- deja a jour, ignore.`);
        skipped++;
        continue;
      }
      if (repo.state === "divergent" || repo.state === "erreur") {
        appendLog(
          `${repo.name} -- ignore (${STATE_LABEL[repo.state]} : ${repo.detail || "a traiter manuellement"})`,
        );
        skipped++;
        continue;
      }
      try {
        // A dirty tree would make pull_repo fail with the dirty-tree
        // guard; behind+dirty repos go straight to the stash-based
        // update so the batch doesn't stall on them one by one.
        const msg =
          repo.state === "non-clone"
            ? await invoke<string>("clone_repo", { name: repo.name })
            : repo.dirty
              ? await invoke<string>("stash_pull_repo", { name: repo.name })
              : await invoke<string>("pull_repo", { name: repo.name });
        appendLog(`${repo.name} -- ${msg}`);
        ok++;
      } catch (e) {
        appendLog(`${repo.name} -- echec : ${String(e)}`);
        failed++;
      }
    }
    setInFlight(null);
    busyRef.current = false;
    setBusy(false);
    setSelected(new Set());
    const parts = [`${ok} reussi(s)`, `${failed} echec(s)`, `${skipped} ignore(s)`];
    if (cancelled) parts.push("annule apres le depot en cours");
    appendLog(`Batch termine : ${parts.join(", ")}.`);
    await refresh();
  };

  const cancelBatch = () => {
    cancelRef.current = true;
    appendLog("Annulation demandee -- arret apres le depot en cours...");
  };

  const updatePackages = async (repo: RepoStatus) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const msg = await invoke<string>("update_packages", { name: repo.name });
      appendLog(`${repo.name} -- ${msg}`);
    } catch (e) {
      appendLog(`${repo.name} -- echec packages : ${String(e)}`);
    }
    busyRef.current = false;
    setBusy(false);
    // These updates leave uncommitted lockfile changes: refresh so the
    // dirty flag (and the stash-based button) reflects reality.
    await refresh();
  };

  const updateFramework = async (repo: RepoStatus) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const msg = await invoke<string>("update_framework", { name: repo.name });
      appendLog(`${repo.name} -- ${msg}`);
    } catch (e) {
      appendLog(`${repo.name} -- echec Vite/Tauri : ${String(e)}`);
    }
    busyRef.current = false;
    setBusy(false);
    await refresh();
  };

  const stashPull = async (repo: RepoStatus) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const msg = await invoke<string>("stash_pull_repo", { name: repo.name });
      appendLog(`${repo.name} -- ${msg}`);
    } catch (e) {
      appendLog(`${repo.name} -- echec : ${String(e)}`);
    }
    busyRef.current = false;
    setBusy(false);
    await refresh();
  };

  const addRepo = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const msg = await invoke<string>("add_repo", {
        name: newRepo.name,
        owner: newRepo.owner,
        path: newRepo.path,
      });
      appendLog(msg);
      setShowAddForm(false);
      setNewRepo({ name: "", owner: "", path: "" });
      await refresh();
    } catch (e) {
      appendLog(`Ajout impossible : ${String(e)}`);
    }
    busyRef.current = false;
    setBusy(false);
  };

  const removeRepo = async (repo: RepoStatus) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const msg = await invoke<string>("remove_repo", {
        name: repo.name,
        owner: repo.owner,
      });
      appendLog(`${repo.name} -- ${msg}`);
      setActiveName(null);
      await refresh();
    } catch (e) {
      appendLog(`${repo.name} -- retrait impossible : ${String(e)}`);
    }
    busyRef.current = false;
    setBusy(false);
  };

  const updateRepoPath = async (repo: RepoStatus, path: string) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const msg = await invoke<string>("update_repo_path", {
        name: repo.name,
        owner: repo.owner,
        path,
      });
      appendLog(`${repo.name} -- ${msg}`);
      setEditingPath(null);
      await refresh();
    } catch (e) {
      appendLog(`${repo.name} -- modification du chemin impossible : ${String(e)}`);
    }
    busyRef.current = false;
    setBusy(false);
  };

  const openInExplorer = async (repo: RepoStatus) => {
    try {
      await invoke("open_in_explorer", { path: repo.path });
    } catch (e) {
      appendLog(`${repo.name} -- ouverture du dossier impossible : ${String(e)}`);
    }
  };

  const selectedRepos = repos.filter((r) => selected.has(r.name));
  const activeRepo = repos.find((r) => r.name === activeName) ?? null;

  // Batch target: the selection if any, otherwise every repo needing
  // work (an all-selected list of up-to-date repos still runs -- pull
  // is idempotent and answers 'deja a jour').
  const primaryTargets = selectedRepos.length > 0 ? selectedRepos : actionableRepos;
  const primaryLabel = busy
    ? "En cours..."
    : selectedRepos.length > 0
      ? `Cloner / mettre a jour la selection (${selectedRepos.length})`
      : actionableRepos.length > 0
        ? `Tout cloner / mettre a jour (${actionableRepos.length})`
        : "Tout est a jour";

  const summary = {
    ok: repos.filter((r) => r.state === "a-jour").length,
    update: repos.filter((r) => r.state === "en-retard").length,
    missing: repos.filter((r) => r.state === "non-clone").length,
    warn: repos.filter((r) => r.state === "divergent" || r.state === "erreur").length,
  };

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="sidebar-header">
          <h1>LibreSync</h1>
        </div>

        <div className="filter-row">
          <input
            className="filter-input"
            placeholder="Filtrer tes depots..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <button
            className="icon-btn"
            onClick={refresh}
            disabled={loading || busy}
            title="Actualiser"
            aria-label="Actualiser"
          >
            {"\u27F3"}
          </button>
          <button
            className="icon-btn"
            onClick={() => setShowAddForm((v) => !v)}
            disabled={loading || busy}
            title="Ajouter un depot"
            aria-label="Ajouter un depot"
          >
            +
          </button>
        </div>

        {showAddForm && (
          <div className="add-form">
            <input
              className="filter-input"
              placeholder="Nom du depot"
              value={newRepo.name}
              onChange={(e) => setNewRepo((f) => ({ ...f, name: e.target.value }))}
            />
            <input
              className="filter-input"
              placeholder="Owner"
              value={newRepo.owner}
              onChange={(e) => setNewRepo((f) => ({ ...f, owner: e.target.value }))}
            />
            <input
              className="filter-input"
              placeholder="Chemin local (ex: W:/mon-depot)"
              value={newRepo.path}
              onChange={(e) => setNewRepo((f) => ({ ...f, path: e.target.value }))}
            />
            <div className="add-form-actions">
              <button disabled={busy} onClick={addRepo}>
                Ajouter
              </button>
              <button disabled={busy} onClick={() => setShowAddForm(false)}>
                Annuler
              </button>
            </div>
          </div>
        )}

        <label className="select-all-row">
          <input
            type="checkbox"
            checked={allVisibleSelected}
            onChange={toggleAll}
            disabled={loading || busy || visibleRepos.length === 0}
          />
          Tout selectionner
          {visibleRepos.length > 0 && (
            <span className="select-all-count">({visibleRepos.length})</span>
          )}
        </label>

        <div className="repo-list">
          {loading && <p className="hint">Verification...</p>}
          {!loading && loadError && (
            <p className="hint hint-error">Erreur de chargement : {loadError}</p>
          )}
          {!loading && !loadError && filteredRepos.length === 0 && (
            <p className="hint">
              {filter ? `Aucun depot ne correspond a "${filter}".` : "Aucun depot."}
            </p>
          )}
          {!loading &&
            filteredRepos.map((repo) => (
              <div
                key={repo.name}
                className={`repo-row${activeName === repo.name ? " active" : ""}`}
                onClick={() => setActiveName(repo.name)}
              >
                <input
                  type="checkbox"
                  checked={selected.has(repo.name)}
                  onChange={() => toggle(repo.name)}
                  onClick={(e) => e.stopPropagation()}
                  disabled={busy}
                />
                {inFlight === repo.name ? (
                  <span className="dot dot-running" title="Operation en cours" />
                ) : (
                  <span className={STATE_DOT[repo.state]} title={STATE_LABEL[repo.state]} />
                )}
                <span className="repo-row-name">{repo.name}</span>
              </div>
            ))}
        </div>

        <div className="sidebar-footer">
          <button
            className="primary full-width"
            onClick={() => runOn(primaryTargets)}
            disabled={busy || primaryTargets.length === 0}
          >
            {primaryLabel}
          </button>
          {busy && (
            <button className="full-width cancel-btn" onClick={cancelBatch}>
              Annuler
            </button>
          )}
        </div>
      </aside>

      <main className="detail-pane">
        {activeRepo ? (
          <>
            <header className="detail-header">
              <h2>{activeRepo.name}</h2>
              <span className={STATE_DOT[activeRepo.state]} />
              <span className="detail-state-label">{STATE_LABEL[activeRepo.state]}</span>
            </header>
            {editingPath === activeRepo.name ? (
              <div className="path-edit">
                <input
                  className="filter-input"
                  value={newRepo.path}
                  onChange={(e) => setNewRepo((f) => ({ ...f, path: e.target.value }))}
                />
                <button disabled={busy} onClick={() => updateRepoPath(activeRepo, newRepo.path)}>
                  Enregistrer
                </button>
                <button disabled={busy} onClick={() => setEditingPath(null)}>
                  Annuler
                </button>
              </div>
            ) : (
              <p className="detail-path">
                {activeRepo.path}{" "}
                <button
                  className="link-btn"
                  disabled={busy}
                  onClick={() => {
                    setNewRepo((f) => ({ ...f, path: activeRepo.path }));
                    setEditingPath(activeRepo.name);
                  }}
                >
                  modifier
                </button>
              </p>
            )}
            {activeRepo.detail && <p className="detail-text">{activeRepo.detail}</p>}
            {activeRepo.dirty && (
              <p className="detail-text">
                Modifications locales non commitees (ex. lockfiles mis a jour par les
                boutons packages / Vite / Tauri) -- elles bloquent la mise a jour simple.
              </p>
            )}
            <div className="detail-actions">
              {activeRepo.state === "en-retard" && activeRepo.dirty ? (
                <button
                  className="primary"
                  disabled={busy}
                  title="git stash, pull --ff-only, puis restauration des modifications locales"
                  onClick={() => stashPull(activeRepo)}
                >
                  {busy ? "En cours..." : "Stasher et mettre a jour"}
                </button>
              ) : (
                ACTIONABLE.includes(activeRepo.state) && (
                  <button className="primary" disabled={busy} onClick={() => runOn([activeRepo])}>
                    {busy ? "En cours..." : actionLabel(activeRepo.state)}
                  </button>
                )
              )}
              {activeRepo.state !== "non-clone" && (
                <button disabled={busy} onClick={() => openInExplorer(activeRepo)}>
                  Ouvrir le dossier
                </button>
              )}
              {activeRepo.state !== "non-clone" && (
                <button disabled={busy} onClick={() => updatePackages(activeRepo)}>
                  {busy ? "En cours..." : "Mettre a jour les packages"}
                </button>
              )}
              {activeRepo.state !== "non-clone" && (
                <button
                  disabled={busy}
                  title="Vite et Tauri vers leurs dernieres versions, meme si ca change de version majeure -- Cargo.lock/package-lock.json restent non commits pour relecture"
                  onClick={() => updateFramework(activeRepo)}
                >
                  {busy ? "En cours..." : "Mettre a jour Vite / Tauri"}
                </button>
              )}
              {confirmRemove ? (
                <>
                  <button className="danger-btn" disabled={busy} onClick={() => removeRepo(activeRepo)}>
                    Confirmer le retrait
                  </button>
                  <button disabled={busy} onClick={() => setConfirmRemove(false)}>
                    Ne pas retirer
                  </button>
                </>
              ) : (
                <button disabled={busy} onClick={() => setConfirmRemove(true)}>
                  Retirer de la liste
                </button>
              )}
            </div>
          </>
        ) : (
          <div className="welcome">
            <h2>Tous tes depots, d'un seul endroit</h2>
            <p className="subtitle">
              Clique un depot a gauche pour le detail, ou utilise la selection pour tout
              mettre a jour d'un coup -- sur cette machine ou une neuve.
            </p>
            <div className="summary-grid">
              <div className="summary-card">
                <span className="dot dot-ok" /> {summary.ok} a jour
              </div>
              <div className="summary-card">
                <span className="dot dot-update" /> {summary.update} mise(s) a jour
              </div>
              <div className="summary-card">
                <span className="dot dot-missing" /> {summary.missing} non clone(s)
              </div>
              {summary.warn > 0 && (
                <div className="summary-card">
                  <span className="dot dot-warn" /> {summary.warn} a verifier
                </div>
              )}
            </div>
          </div>
        )}

        {log.length > 0 && (
          <section className="log">
            <h3>Journal</h3>
            <ul>
              {log.map((entry, i) => (
                <li key={i}>{entry}</li>
              ))}
            </ul>
          </section>
        )}
      </main>
    </div>
  );
}

export default App;
