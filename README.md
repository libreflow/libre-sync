# LibreSync

Hub desktop multi-repos GitHub : clone et met à jour tous les dépôts d'un compte GitHub depuis une seule interface (Tauri 2 + React 19 + TypeScript).

## Prérequis

- [git](https://git-scm.com/)
- [gh](https://cli.github.com/) (CLI GitHub, authentifiée) — optionnel mais recommandé (clone via `gh repo clone`, découverte automatique des nouveaux dépôts)
- [Node.js](https://nodejs.org/) + npm (pour `npm update`, `npm run build`)
- [Rust](https://rustup.rs/) + cargo (pour `cargo update`, `cargo check`)
- Windows : `explorer` pour ouvrir un dossier ; macOS : `open` ; Linux : `xdg-open`

## Développement

```sh
npm install
npm run tauri dev
```

## Build

```sh
npm run tauri build
```

## Configuration

Au premier lancement, un fichier `repos.json` est créé dans le dossier de config de l'app (`%APPDATA%/com.robinsonx.libre-sync/` sur Windows, `~/Library/Application Support/...` sur macOS, `~/.config/...` sur Linux). Il contient :

```json
{
  "defaultBaseDir": "W:/",
  "defaultOwner": "libreflow",
  "repos": [{ "name": "mon-repo", "owner": "libreflow", "path": "W:/mon-repo" }]
}
```

- `defaultBaseDir` : racine où les nouveaux dépôts découverts sur GitHub sont clonés par défaut.
- `defaultOwner` : compte GitHub scanné par `gh repo list` à chaque rafraîchissement ; tout dépôt distant absent de `repos` y est ajouté automatiquement (le chemin d'une entrée existante n'est jamais modifié).
- `repos[].path` : emplacement local du clone ; `"non-clone"` s'affiche s'il n'existe pas encore.

## États des dépôts

| État        | Signification                                | Action                         |
| ----------- | -------------------------------------------- | ------------------------------ |
| `a-jour`    | Identique à l'amont                          | —                              |
| `en-retard` | Des commits à récupérer                      | `git pull --ff-only`           |
| `non-clone` | Pas de clone local                           | `gh repo clone` ou `git clone` |
| `divergent` | Commits locaux non poussés                   | À traiter manuellement         |
| `erreur`    | Pas d'amont configuré / vérification échouée | À vérifier                     |

## Philosophie de mise à jour

- **Mise à jour des packages** (`npm update`, `cargo update`) : reste dans les plages de versions déclarées (`^`/`~`), jamais de bump majeur.
- **Mise à jour Vite/Tauri** (`npm install vite@latest …`, `cargo add tauri@latest …`) : suit les dernières versions, majeures incluses, puis vérifie (`npm run build`, `cargo check`) que le projet compile toujours.
- **Jamais de commit ni de push** : les lockfiles modifiés (`package-lock.json`, `Cargo.lock`) restent dans l'arbre de travail pour relecture et test avant commit manuel. Un `git checkout -- package-lock.json` suffit pour annuler.
- `pull_repo` refuse de s'exécuter si l'arbre de travail contient des modifications non enregistrées.

## Tests

```sh
# Frontend (typecheck)
npx tsc --noEmit

# Backend (tests unitaires sur la logique pure)
cd src-tauri && cargo test
```
