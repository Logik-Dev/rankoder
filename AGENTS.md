# AGENTS.md

Instructions pour les agents de code (OpenCode, etc.) travaillant dans ce dépôt.
À lire **avant toute modification**.

## Règles d'or (non négociables)

1. **Atomicité.** Une étape = un seul changement logique cohérent. Jamais de
   mélange refactor + feature + fix dans la même étape. Si une tâche implique
   plusieurs changements indépendants, les découper en étapes séquentielles et
   les appliquer **une par une**.
2. **Vérification avant de déclarer "terminé".** Une étape n'est finie que
   lorsque `cargo clippy --all-targets --all-features -- -D warnings` et
   `cargo test` passent (voir *Commandes*).
3. **Commit conventionnel proposé après chaque étape.** Une fois l'étape
   atomique appliquée et vérifiée, **proposer** un message de commit au format
   Conventional Commits (voir section dédiée). Ne **pas** committer
   automatiquement : présenter le message pour validation humaine.
4. **Pas de `unwrap()` / `expect()` / `panic!`** hors tests et hors setup
   `main` explicitement justifié.
5. **Ne rien inventer.** API, trait ou signature incertaine → vérifier dans le
   code / `Cargo.toml` / la doc avant d'écrire. Pas de DSL ni de méthode
   hallucinée.

## Boucle de travail

Pour chaque tâche :

1. **Comprendre** : lire les fichiers concernés, ne pas deviner.
2. **Planifier** : annoncer la découpe en étapes atomiques.
3. Pour chaque étape :
   1. Appliquer le changement minimal.
   2. `cargo check` → `cargo clippy … -D warnings` → `cargo test` (au moins le test ciblé).
   3. Corriger jusqu'au vert.
   4. **Proposer** le message de commit conventionnel.
4. Passer à l'étape suivante uniquement quand la précédente est verte **et** son
   commit proposé.

## Conventions Rust

### Erreurs
- Domaine / bibliothèque : erreurs typées avec `thiserror`, hiérarchie explicite.
- Frontières applicatives / `main` / bin : `anyhow` (ou `eyre`) avec `.context(…)`.
- Propager avec `?`. Pas de `String` comme type d'erreur.

### Types
- Newtypes plutôt que primitifs nus pour identifiants et valeurs métier
  (ex. `UserId(Uuid)`), avec `derive` minimal pertinent.
- Value objects validés à la construction (`TryFrom` / `FromStr`) : invariants
  impossibles à violer une fois l'objet construit.
- Type-state pattern pour les machines à états vérifiées à la compilation.
- `#[non_exhaustive]` sur les enums/struct publics susceptibles d'évoluer.

### Async
- Pas de blocage du runtime (`std::fs`, `std::thread::sleep`, calcul lourd) dans
  une tâche async : équivalents async ou `spawn_blocking`.
- Annulation propre : `CancellationToken`, `select!`, drainage des `JoinSet`.
- Pas de `.await` en tenant un `Mutex` `std` (utiliser un mutex async si besoin).

### Style & modules
- `rustfmt` fait foi (respecter le `rustfmt.toml` du repo, ex. `imports_granularity`).
- Pas de glob `use *` hors préludes / tests.
- Visibilité minimale : `pub(crate)` par défaut, `pub` seulement à la frontière publique.
- Documenter les items publics (`///`), exemples compilables si pertinent.

## Architecture

Quand le projet suit une architecture en couches / hexagonale :
- Le domaine ne dépend d'aucune infra (DB, HTTP, FS) ; les dépendances pointent
  **vers** le domaine.
- Ports = traits dans le domaine ; adapters = implémentations dans l'infra.
- Aucun type d'infra (`sqlx::PgPool`, types HTTP, etc.) ne fuit dans les
  signatures du domaine.
- **Observer l'organisation existante avant d'imposer un pattern.**

## Tests
- Tout changement de comportement s'accompagne d'un test (unitaire au plus près,
  intégration à la frontière).
- Déterministes, isolés, sans dépendance réseau non mockée.
- Nommés par comportement attendu ; assertions précises plutôt que fourre-tout.

## Performance
- **Mesurer avant d'optimiser** : `criterion` sur les chemins chauds, pas
  d'optimisation à l'aveugle.
- Éviter allocations/clones inutiles : emprunts, `Cow`, itérateurs paresseux.
- Signaler les coûts algorithmiques évidents sans micro-optimiser prématurément.

## Commandes de vérification

Adapter aux outils réellement présents dans le dépôt.

```sh
cargo check  --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo fmt --check
```

Dépôt sous flake Nix : préférer les commandes du flake quand elles existent
(`nix flake check`, `nix develop -c cargo …`). Pas de chemins impurs.

## Dépôts polyglottes
- **Frontend (TS / Angular)** : typage strict, pas de `any`, signaux & reactive
  forms typés. Lancer `tsc --noEmit`, lint et tests du package concerné.
- **Nix** : ne pas casser le flake ; `nix flake check` doit passer.
- L'**atomicité** et le **commit conventionnel** s'appliquent à tous les langages.

## Format des messages de commit (Conventional Commits)

Après chaque étape atomique vérifiée, proposer :

```
<type>(<scope>): <description impérative courte>

<corps optionnel : le pourquoi, pas le comment>

<footer optionnel : BREAKING CHANGE:, refs>
```

Types : `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`,
`chore`, `style`, `revert`.

Règles :
- Description ≤ ~72 caractères, impératif présent ("add", pas "added").
- Un commit = une étape atomique = un seul `type`.
- Changement cassant : suffixe `!` (`feat(api)!: …`) **et** footer `BREAKING CHANGE: …`.
- `scope` = module / crate concerné quand pertinent.

Exemples :

```
feat(domain): add Bitrate value object with TryFrom validation
fix(scanner): handle symlink loops in parallel walk
refactor(repo): extract MediaRepository port from sqlx impl
perf(transcode): avoid per-frame allocation in probe parser
test(domain): cover Resolution parsing edge cases
```

> **Jujutsu (`jj`).** Le message proposé s'applique via
> `jj describe -m "…"` sur la révision courante, puis `jj new` pour l'étape
> suivante. Ne **pas** exécuter `jj` / `git` commit sans validation explicite.
> (Commits en anglais par défaut — localiser si le projet l'exige.)
