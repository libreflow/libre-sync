# AGENTS.md — libre-sync

Règles de référence pour tout agent de code IA travaillant sur ce repo.

## Projet
Hub multi-dépôts GitHub — clone/pull, mises à jour de packages et de Vite/Tauri, depuis une app Tauri 2 + React (front TS, back Rust exécutant git/gh/npm/cargo).

## Commandes
- `npm run dev` — dev (Vite + Tauri)
- `npm run build` — `tsc && vite build`
- `npm run format:check` / `npm run format` — prettier
- Rust : `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` depuis `src-tauri`

## Règles Tauri 2 (non négociables)
- Toute commande IPC Rust est `async fn` et déclarée dans `generate_handler!` (les commandes synchrones bloquent le thread principal et gèlent l'UI).
- Capabilities au moindre privilège : pas d'élargissement de permissions ni de wildcard sans justification dans la PR.
- Ne pas modifier `tauri.conf.json` sans accord explicite du mainteneur.
- Un clone échoué doit être nettoyé (pas de pseudo-clone à moitié écrit) ; jamais cloner dans un chemin préexistant.

## Sécurité
- Jamais de secret ni de clé d'API dans le code ; les tokens passent par la config système de `gh`/git.
- Toute nouvelle dépendance (npm ou crate) doit être auditée avant ajout.

## Workflow
- Une tâche = une issue = une petite PR focalisée.
- Spécification courte (contexte + critères d'acceptation) écrite avant de coder.
- CI verte obligatoire avant tout merge ; vérification manuelle (`npm run tauri dev`) requise avant toute release.
