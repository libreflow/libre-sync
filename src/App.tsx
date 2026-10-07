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

function actionLabel(state: RepoState): string {
  switch (state) {
    case "non-clone":
      return "Cloner";
    case "en-retard":
      return "Mettre a jour";
    case "a-jour":
      return "A jour";
    case "divergent":
      return "A traiter manuellement";
    default:
      return "Verifier";
  }
}

function App() {
  const [repos, setRepos] = useState<RepoStatus[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [activeName, setActiveName] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [log, setLog] = useState<string[]>([]);
  // Synchronous guard: `busy` (React state) only reflects on the next
  // render, so a double-click before that re-render would start a
  // second concurrent clone/pull loop. This ref flips in the same tick.
  const busyRef = useRef(false);

  const appendLog = useCallback((entry: string) => {
    setLog((l) => [entry, ...l.slice(0, LOG_MAX_ENTRIES - 1)]);
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const result = await invoke<RepoStatus[]>("list_repo_status");
      result.sort((a, b) => a.name.localeCompare(b.name));
      setRepos(result);
    } catch (e) {
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
  const allActionableSelected =
    actionableRepos.length > 0 && actionableRepos.every((r) => selected.has(r.name));

  const toggle = (name: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const toggleAll = () => {
    setSelected(allActionableSelected ? new Set() : new Set(actionableRepos.map((r) => r.name)));
  };

  const runOn = async (targets: RepoStatus[]) => {
    if (targets.length === 0 || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    for (const repo of targets) {
      try {
        const msg =
          repo.state === "non-clone"
            ? await invoke<string>("clone_repo", { name: repo.name })
            : await invoke<string>("pull_repo", { name: repo.name });
        appendLog(`${repo.name} -- ${msg}`);
      } catch (e) {
        appendLog(`${repo.name} -- echec : ${String(e)}`);
      }
    }
    busyRef.current = false;
    setBusy(false);
    setSelected(new Set());
    await refresh();
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
  };

  const openInExplorer = async (repo: RepoStatus) => {
    try {
      await invoke("open_in_explorer", { path: repo.path });
    } catch (e) {
      appendLog(`${repo.name} -- ouverture du dossier impossible : ${String(e)}`);
    }
  };

  const selectedActionable = repos.filter(
    (r) => selected.has(r.name) && ACTIONABLE.includes(r.state),
  );
  const activeRepo = repos.find((r) => r.name === activeName) ?? null;

  const primaryTargets = selectedActionable.length > 0 ? selectedActionable : actionableRepos;
  const primaryLabel = busy
    ? "En cours..."
    : selectedActionable.length > 0
      ? `Mettre a jour la selection (${selectedActionable.length})`
      : actionableRepos.length > 0
        ? `Tout mettre a jour (${actionableRepos.length})`
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
        </div>

        <label className="select-all-row">
          <input
            type="checkbox"
            checked={allActionableSelected}
            onChange={toggleAll}
            disabled={loading || busy || actionableRepos.length === 0}
          />
          Tout selectionner
          {actionableRepos.length > 0 && (
            <span className="select-all-count">({actionableRepos.length})</span>
          )}
        </label>

        <div className="repo-list">
          {loading && <p className="hint">Verification...</p>}
          {!loading && filteredRepos.length === 0 && <p className="hint">Aucun depot.</p>}
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
                  disabled={busy || !ACTIONABLE.includes(repo.state)}
                />
                <span className={STATE_DOT[repo.state]} title={STATE_LABEL[repo.state]} />
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
            <p className="detail-path">{activeRepo.path}</p>
            {activeRepo.detail && <p className="detail-text">{activeRepo.detail}</p>}
            <div className="detail-actions">
              {ACTIONABLE.includes(activeRepo.state) && (
                <button className="primary" disabled={busy} onClick={() => runOn([activeRepo])}>
                  {busy ? "En cours..." : actionLabel(activeRepo.state)}
                </button>
              )}
              {activeRepo.state !== "non-clone" && (
                <button onClick={() => openInExplorer(activeRepo)}>
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
