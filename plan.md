Le code est globalement sain — découpage par modules clair, IDs typés, traits aux bonnes frontières (`ApprovalNotifier`, providers), transactions systématiques. Voici mes propositions, classées par impact.

## Robustesse (le plus important)

**1. Les événements `pg_notify` perdus ne sont jamais rattrapés.**
`NOTIFY` est fire-and-forget : si l'app est down (ou si le listener est en train de se reconnecter, cf. la boucle de retry de `listener.rs`) quand le trigger tire, le fichier reste bloqué dans son état pour toujours. Tu as déjà la table `events` — il manque juste le rattrapage :
- au démarrage (et après chaque reconnexion du listener), scanner les `media_files` dans un état non-terminal (`discovered`, `probed`, `analyzed`) et les injecter dans le canal mpsc ;
- optionnellement, tracker un `last_processed_event_id` pour faire un vrai outbox pattern.

C'est le changement qui transforme le daemon de "fonctionne tant que rien ne tombe" en "converge toujours".

**2. Les transitions d'état ne sont pas protégées contre les races.**
Tous les `UPDATE media_files SET workflow_state = …` sont inconditionnels. Deux notifications pour le même fichier (ou un replay au rattrapage) peuvent faire avancer l'état deux fois ou le faire reculer. Proposition : compare-and-swap en SQL —

```sql
UPDATE media_files SET workflow_state = 'probed', …
WHERE id = $1 AND workflow_state = 'discovered'
```

puis vérifier `rows_affected() == 1` ; si 0, logger et abandonner. Ça rend tout le pipeline idempotent, ce qui est exactement ce qu'il faut combiné au point 1.

**3. Le publish MQTT peut se bloquer silencieusement.**
Avec `rumqttc`, un `publish` ne part que si l'eventloop est pollée. Or elle n'est pollée que dans `listen_responses` — et si celle-ci sort en erreur (le `spawn` dans `run_response_listener` logge puis meurt), plus personne ne polle : les `request_approval` suivants s'empilent dans le buffer sans jamais partir, sans erreur. Contrairement à `PostgresListener`, il n'y a pas de boucle de reconnexion. Proposition : une tâche dédiée "driver" qui polle l'eventloop en boucle avec backoff (comme le listener Postgres), séparée de la logique d'abonnement. Ça supprime aussi le `Mutex<EventLoop>` qui est un signe que deux responsabilités cohabitent mal.

**4. L'état `failed` existe mais n'est jamais utilisé.**
Quand `ffprobe` échoue, on `continue` : le fichier reste en `discovered` sans trace en base (juste un log). Proposition : transitionner vers `failed` avec un événement `MediaEvent::ProbeFailed { error }`, ou un compteur de retries. Sinon impossible de distinguer "pas encore traité" de "échoue à chaque fois".

## Design / structure

**5. La machine à états est éparpillée.**
Les transitions vivent à trois endroits : le dispatch dans `WorkflowOrchestrator`, les `UPDATE` dans `MediaStore`, et les guards dans `TakeTranscodeDecisionService` (qui re-vérifie `workflow_state`, ce qui est une fuite de responsabilité — un service de décision pur ne devrait pas connaître le workflow). Proposition : centraliser les transitions valides sur l'enum lui-même (`WorkflowStateTag::next_on(event)` ou une table de transitions), et que le store expose une seule méthode générique au lieu de quatre quasi identiques :

```rust
async fn transition(&self, id: &MediaFileId, from: WorkflowStateTag,
                    to: WorkflowStateTag, event: MediaEvent) -> Result<(), StoreError>
```

`save_pending_approval`, `save_approval_granted`, `save_approval_rejected` et la moitié de `save_analysis_result` deviennent des appels d'une ligne, et le CAS du point 2 est implémenté une seule fois.

**6. La table `events` sert à la fois d'audit log et de source de données.**
`fetch_approval_info` extrait `compression_potential` du JSONB des events via une sous-requête sur `event->>'type' = 'analyzed'` — fragile (couplage par chaîne, dépend de l'ordre). Proposition : stocker le résultat d'analyse complet dans `transcode_spec` (crf + bpp + compression_potential), et réserver `events` à l'audit. Bonus : `info.crf.unwrap_or(24)` dans `ApprovalOrchestrator` masque une donnée manquante — si l'état est `analyzed`, le spec doit exister, sinon c'est une erreur, pas un défaut silencieux.

**7. `FFmpeg` n'est pas derrière un trait, contrairement au notifier.**
`WorkflowOrchestrator` garde un champ `_ffmpeg: FFmpeg` inutilisé et appelle `FFmpeg::probe` statiquement. Incohérent avec `ApprovalNotifier`, et ça rend l'orchestrateur intestable. Proposition : un trait `Prober` (`async fn probe(&self, path) -> Result<VideoProperties>`) injecté comme le notifier. Même logique à terme pour un trait côté store si tu veux unit-tester les orchestrateurs sans Postgres (sinon, `#[sqlx::test]` fait très bien le travail en intégration).

## Organisation / bonnes pratiques

**8. Config unifiée et fail-fast.**
`Config`, `AnalysisConfig`, `MqttConfig` sont chargées séparément dans `main`, et `DATABASE_URL`/`AUTO_MIGRATE` sont lues en direct via `env::var`. Surtout, `parse_env` avale les erreurs de parsing : `MQTT_PORT=abcd` donne silencieusement 1883. Proposition : une struct `AppConfig` unique avec un seul `from_env()` qui nomme la variable fautive et échoue sur une valeur invalide (distinguer "absente → défaut" de "présente mais invalide → erreur").

**9. Arrêt propre.**
Le `tokio::select!` sur Ctrl+C sort de `main` sans annuler les tâches — un transcodage futur serait tué en plein vol. Proposition : `tokio_util::sync::CancellationToken` passé aux orchestrateurs + `JoinSet` pour attendre leur arrêt. Pas urgent maintenant, indispensable avant l'étape `transcoding`.

**10. Petites choses.**
- `EventNotification` dans `listener.rs` est mort : le payload est parsé à la main via `serde_json::Value`. Utiliser la struct (avec `media_file_id: Uuid` directement) simplifie le parsing et supprime le code mort.
- `std::mem::forget(guard)` dans `init_tracing` : retourner le guard et le garder vivant dans `main` est l'idiome attendu (et garantit le flush des logs à l'arrêt).
- `TakeTranscodeDecisionService::execute` est pur et bourré de seuils — c'est le candidat n°1 pour les premiers tests unitaires du projet (table-driven sur bpp/résolution/rating).
- Le `WorkflowOrchestrator` traite séquentiellement : un ffprobe à la fois. Pour une grosse bibliothèque, un `JoinSet` borné par un sémaphore (4-8 probes concurrents) accélérerait beaucoup la phase initiale — à faire après le point 2, car la concurrence sans CAS aggrave les races.

## Par où commencer

Si je devais ordonner : **2 → 5 → 1** forment un tout cohérent (transitions idempotentes centralisées, puis rattrapage au démarrage qui peut rejouer sans danger), ensuite **3** (fiabilité MQTT, critique puisque c'est le point d'approbation humaine), puis le reste au fil de l'eau. Dis-moi si tu veux que j'implémente un de ces blocs.



  Deux vrais problèmes restants, tous côté MQTT

  1. La souscription est perdue après une reconnexion (bug).
  listen_responses souscrit une seule fois au démarrage. rumqttc reconnecte automatiquement au prochain poll() après une coupure, mais avec clean_session = true (le défaut de MqttOptions), le broker oublie la
  souscription à la reconnexion — et personne ne re-souscrit. Résultat : après la première coupure réseau/redémarrage du broker, les réponses d'approbation n'arrivent plus jamais, silencieusement. Le fix standard :
  dans la boucle driver de MqttNotifier::new, re-souscrire sur chaque Event::Incoming(Packet::ConnAck(_)) (ce qui couvre aussi la souscription initiale, et tu peux alors retirer le subscribe de listen_responses).

  2. Pas de backoff dans la boucle driver.
  Sur erreur de connexion persistante (broker down), eventloop.poll() retourne Err immédiatement à chaque itération → busy-loop qui mange un cœur et inonde les logs de warnings. Ajoute un
  tokio::time::sleep(Duration::from_secs(1)) dans la branche Err, comme tu l'as fait pour PostgresListener.

  Points mineurs (à noter, pas bloquants)

  - ApprovalInfo.bpp est fetché mais jamais lu (ApprovalRequest n'a pas de champ bpp) — code mort à retirer ou champ à exposer.
  - Dans la boucle du workflow, le acquire_owned().await est dans le corps de la branche recv : si tous les permits sont pris, la cancellation n'est observée qu'après obtention d'un permit. Sans conséquence avec des
  ffprobes courts, mais à garder en tête quand les tâches deviendront des transcodages de 2h.
  - Un fichier en pending_approval dont la réponse MQTT se perd reste bloqué pour toujours (le catch-up le re-énumère mais le workflow l'ignore). À terme : un timeout qui re-publie la demande, ou un état "stale
  approval".
  - La tâche driver MQTT spawnée dans new() est détachée — elle ne participe pas au shutdown gracieux. Acceptable aujourd'hui (le process exit la tue), mais incohérent avec le reste.
  - Le dernier point de ma revue reste ouvert : decision.rs est pur et testable, et il n'y a toujours que les 2 tests serde. C'est le meilleur ratio effort/valeur pour démarrer la suite de tests.

  Si tu corriges le point 1 (re-souscription sur ConnAck) et le backoff, la couche MQTT sera au niveau du reste. Dis-moi si tu veux que je m'en charge.


  Broutilles

  - La VecDeque pending est non bornée : la backpressure que fournissait le canal mpsc(100) a disparu, le listener peut maintenant y déverser tout le catch-up d'un coup. Comme ce ne sont que des UUID (16 octets),
  c'est négligeable en pratique — juste à savoir.
  - Le seuil de staleness est codé en dur (5 dans main.rs:96) alors que tous les autres seuils sont dans AppConfig. À déplacer pour rester cohérent (APPROVAL_STALE_MINUTES ?).
  - ApprovalInfo.bpp est toujours fetché et jamais lu — toujours du code mort.
  - Les tests unitaires de decision.rs manquent toujours — ça reste le meilleur point d'entrée pour la suite de tests.

  Le seul point qui mérite une correction avant de continuer, c'est le ? du stale checker. Tu veux que je le corrige (et éventuellement les broutilles avec) ?


# Plan — File d'approbation bornée (au plus K en attente)

## Principe

Aujourd'hui le déclenchement est *eager* : chaque fichier qui atteint `analyzed` appelle `send_request` immédiatement (`workflow.rs:170`), d'où le fan-out. On inverse en **pull** : un seul « feeder » promeut des fichiers `analyzed → pending_approval` tant qu'il reste des slots (`K - nb_pending`). La DB reste la source de vérité → robuste au redémarrage.

**Décision de design à acter :** `K` borne le **nombre de décisions humaines en attente** (`workflow_state = 'pending_approval'`), pas le nombre de transcodages en vol. Conséquence : dès que tu approuves (→ `transcoding`), un slot se libère et la demande suivante part. L'approbation est ainsi découplée de la durée du transcodage. (Alternative si un jour tu veux « ne pas approuver tant que le transcode courant n'est pas fini » : compter `pending_approval + transcoding` — non retenu ici car le worker de transcodage n'existe pas encore.)

## Changements par fichier

### 1. `src/store/mod.rs` — deux requêtes
```rust
pub async fn count_pending_approvals(&self) -> Result<i64, StoreError>
// SELECT COUNT(*) FROM media_files WHERE workflow_state = 'pending_approval'

pub async fn fetch_oldest_analyzed(&self, limit: i64) -> Result<Vec<MediaFileId>, StoreError>
// SELECT id FROM media_files WHERE workflow_state = 'analyzed'
// ORDER BY updated_at ASC LIMIT $1
```
Pas de nouvelle migration : l'index partiel `idx_media_files_active` couvre les filtres.

### 2. `src/approval/mod.rs` — le feeder
- Ajouter un champ `wake: tokio::sync::Notify` à `ApprovalOrchestrator` (l'orchestrateur est déjà dans un `Arc`, pas besoin de l'envelopper).
- Méthode publique `pub fn wake_feeder(&self) { self.wake.notify_one(); }`.
- Dans `handle_response`, **après** une transition réussie (granted *ou* rejected → un slot se libère), appeler `self.wake.notify_one()`.
- Nouvelle boucle :
```rust
pub async fn run_approval_feeder(self: Arc<Self>, token: CancellationToken, capacity: usize) -> Result<()> {
    loop {
        self.top_up(capacity).await;   // top-up immédiat au démarrage
        tokio::select! {
            biased;
            _ = token.cancelled() => return Ok(()),
            _ = self.wake.notified() => {}
        }
    }
}

async fn top_up(&self, capacity: usize) {
    let pending = match self.store.count_pending_approvals().await { Ok(n) => n, Err(e) => { error!(...); return; } };
    let slots = capacity as i64 - pending;
    if slots <= 0 { return; }
    let ids = match self.store.fetch_oldest_analyzed(slots).await { ... };
    for id in ids {
        let Ok(mf) = self.store.find_media_file_by_id(&id).await else { continue; };
        if let Err(e) = self.send_request(&mf).await { error!(?id, %e, "feeder send failed"); }
    }
}
```
`send_request` reste tel quel (transition `Analyzed → PendingApproval` + publish, avec `StaleState` toléré). Le feeder étant **une seule tâche séquentielle**, il n'y a aucune race sur le comptage des slots.

> Idiome `Notify` : `notify_one` bufférise un permit (max 1). Un réveil arrivant *pendant* `top_up` est mémorisé → le `notified()` suivant repart aussitôt. Pas de réveil perdu.

### 3. `src/workflow.rs` — ne plus pousser, juste réveiller
Remplacer le bras `Analyzed` (`workflow.rs:169-173`) :
```rust
WorkflowStateTag::Analyzed => approval.wake_feeder(),
```
C'est le hook « un nouveau fichier analysé existe ». La transition `Probed → Analyzed` continue de se faire dans `analysis.analyze()` comme aujourd'hui.

### 4. `src/config.rs` — le knob
Ajouter `approval_max_pending: usize`, lu via `parse_env("APPROVAL_MAX_PENDING", 1)`. Défaut **1** = strictement une à la fois.

### 5. `src/main.rs` — câblage
Remplacer/compléter le spawn :
```rust
join_set.spawn(approval_orchestrator.clone().run_approval_feeder(token.child_token(), cfg.approval_max_pending));
join_set.spawn(approval_orchestrator.clone().run_stale_checker(token.child_token(), 5));
```
Le `run_stale_checker` est conservé tel quel : avec K petit il ne re-publie qu'au plus K demandes → plus de flood.

## Effets

- HA n'affiche jamais plus de **K** cartes → validation une par une, sans rafale.
- Granularité par fichier **préservée**.
- **Crash-safe** : au démarrage le feeder lit l'état réel (gère aussi les `analyzed` orphelins d'un run précédent — bonus vs le design actuel qui ne les rejouerait jamais sans nouvel event).
- Back-pressure naturel : rien ne part tant que les slots sont pleins.

## Points de vigilance / ordre

1. `count_pending_approvals` + `fetch_oldest_analyzed` (store, vérifiables isolément).
2. Feeder + `wake_feeder` + notify dans `handle_response`.
3. Bascule `workflow.rs` (retirer `send_request` ici — sinon double promotion concurrente).
4. Config + câblage `main.rs`.
5. `cargo build` (sqlx en compile-time → DB live nécessaire) + `cargo clippy`.



  Ce qui est bon

  - store/mod.rs : count_pending_approvals (avec unwrap_or(0) sur le COUNT nullable — correct) et fetch_oldest_analyzed (ORDER BY updated_at ASC) sont justes.
  - approval/mod.rs : le feeder top_up calcule slots = capacity - pending, court-circuite si <= 0, et send_request reste le seul promoteur. Comme le feeder est une tâche unique séquentielle, aucune race sur le
  comptage des slots. 👍
  - handle_response réveille le feeder (notify_one) après transition → un slot libéré relance immédiatement la suivante. Le notify_one même sur le chemin StaleState est inoffensif (juste un top_up no-op).
  - Sémantique Notify correcte : notify_one bufférise un permit, donc un réveil arrivant pendant top_up n'est pas perdu — le notified() suivant repart aussitôt. Pas de wakeup manqué.
  - workflow.rs : bras Analyzed → wake_feeder(), plus de send_request eager. La transition PendingApproval reste un no-op → pas de boucle d'événements.
  - main.rs / config.rs : câblage et APPROVAL_MAX_PENDING (défaut 1) corrects.

  Deux remarques mineures (pas bloquantes)

  1. Head-of-line blocking latent dans top_up (approval/mod.rs:128-135) : si send_request échoue avant la transition (c.-à-d. build_request qui bail! sur compression_potential/video_properties manquants), le fichier
  reste analyzed et redevient l'« oldest » au prochain réveil → il bloquerait toute la file derrière lui. En pratique ça ne peut pas arriver (un fichier Analyzed vient d'une décision Encode qui inclut toujours
  compression_potential, et Probed garantit les video_properties), donc c'est purement une fragilité. Si tu veux durcir : transitionner ces cas vers Failed plutôt que de les laisser en analyzed.
  2. Erreur silencieuse (approval/mod.rs:129) : let Ok(media_file) = ... else { continue; } avale l'erreur de find_media_file_by_id sans log, contrairement au run_stale_checker qui logge. Pour la cohérence, un
  error!(?id, ...) serait bienvenu.

  Aucune des deux n'est un bug réel sur le flux actuel — l'implémentation est prête.

Parfait. Voici le plan détaillé. Rappel du cadre : **grain = saison** (`series_id` + `season_number`), **films individuels** (lot de 1), **tout-ou-rien**, **déclenchement par complétude** (une saison part quand tous ses épisodes ont quitté `discovered`/`probed`), **K borne le nombre de lots en attente**. Le point clé : **aucune migration** — la clé de lot est dérivée des métadonnées.

## 1. Un type `BatchKey` (nouveau, `src/models/batch.rs`)

L'identité d'un lot, encodable en string pour MQTT et décodable au retour :

```rust
pub enum BatchKey {
    Season { series_id: SeriesId, season: i16 },
    Movie  { movie_id: MovieId },
}
// encode : "series:{uuid}:s{n}"  |  "movie:{uuid}"
// decode : split(':') → discriminant + UUID (+ saison sans le 's')
```

C'est ce qui voyage dans `batch_id` côté MQTT, et ce que `handle_response` reparse. Tout le module approval bascule de `MediaFileId` vers `BatchKey`.

## 2. Le store (`src/store/mod.rs`) — requêtes dérivées, 0 schéma

**a. `fetch_ready_batch_keys(limit) -> Vec<BatchKey>`** — le cœur. UNION des saisons prêtes et des films prêts, FIFO :

```sql
SELECT 'season' AS kind, e.series_id, e.season_number AS season, NULL::uuid AS movie_id,
       MIN(mf.updated_at) AS oldest
FROM media_files mf JOIN episodes e ON mf.episode_id = e.id
GROUP BY e.series_id, e.season_number
HAVING bool_or(mf.workflow_state = 'analyzed')                                  -- ≥1 à approuver
   AND NOT bool_or(mf.workflow_state IN ('discovered','probed','pending_approval'))
                                       -- complétude + pas déjà un lot en attente sur cette saison
UNION ALL
SELECT 'movie', NULL, NULL, mf.movie_id, mf.updated_at
FROM media_files mf
WHERE mf.movie_id IS NOT NULL AND mf.workflow_state = 'analyzed'
ORDER BY oldest ASC
LIMIT $1
```

La règle `NOT bool_or(... 'pending_approval')` évite de re-promouvoir une saison dont un lot attend déjà une décision. La complétude (`discovered/probed` absents) garantit qu'on n'envoie pas une saison à moitié analysée. `failed` étant terminal, un épisode qui échoue au probe ne bloque pas la saison.

**b. `count_pending_batches() -> i64`** — pour le calcul des slots :

```sql
SELECT (SELECT COUNT(DISTINCT (e.series_id, e.season_number))
        FROM media_files mf JOIN episodes e ON mf.episode_id=e.id
        WHERE mf.workflow_state='pending_approval')
     + (SELECT COUNT(*) FROM media_files
        WHERE movie_id IS NOT NULL AND workflow_state='pending_approval')
```

**c. `transition_batch(key, from, to, event) -> Vec<MediaFileId>`** — transition atomique de tous les membres, en une transaction, avec insertion groupée des events :

```sql
-- saison
UPDATE media_files mf SET workflow_state=$to
FROM episodes e
WHERE mf.episode_id=e.id AND e.series_id=$1 AND e.season_number=$2
  AND mf.workflow_state=$from
RETURNING mf.id
-- puis : INSERT INTO events(media_file_id, event) SELECT unnest($ids), $event
```

Renvoie les ids réellement transitionnés (0 = race / déjà traité → on n'envoie rien).

**d. `fetch_batch_request_info(key) -> BatchApprovalInfo`** — l'agrégat pour la notif, calculé sur les membres `pending_approval` :

```sql
-- saison
SELECT s.title, s.rating,
       COUNT(*) AS file_count,
       SUM(mf.size_bytes) AS total_size_bytes,
       SUM(mf.size_bytes * LEAST(GREATEST(COALESCE((mf.transcode_spec->>'compression_potential')::float8,0),0),1)) AS saved_bytes
FROM media_files mf JOIN episodes e ON mf.episode_id=e.id JOIN series s ON e.series_id=s.id
WHERE e.series_id=$1 AND e.season_number=$2 AND mf.workflow_state='pending_approval'
GROUP BY s.title, s.rating
```

**e. `fetch_stale_pending_batches(threshold_minutes) -> Vec<BatchKey>`** — comme `fetch_stale_pending_approvals` mais groupé par lot (min(updated_at) du lot < seuil).

→ **À supprimer** (devenus inutiles) : `fetch_oldest_analyzed`, `count_pending_approvals`, `fetch_stale_pending_approvals`, `fetch_approval_info`. `transition` (single) reste (utilisé par probe/analyse).

## 3. Les messages MQTT (`src/notification/mod.rs`)

```rust
pub struct ApprovalRequest {
    pub batch_id: String,            // BatchKey encodé
    pub title: String,               // "Breaking Bad — Saison 2" | titre film
    pub file_count: u32,
    pub total_size_gb: f64,
    pub total_space_saved_gb: f64,
    pub tmdb_rating: Option<f32>,
}
pub struct ApprovalResponse { pub batch_id: String, pub approved: bool }
```

`mqtt.rs` est inchangé (il sérialise la struct, point). Le titre saison est construit côté approval (`format!("{} — Saison {}", series_title, season)`), le film garde son titre.

## 4. Le module approval (`src/approval/mod.rs`)

- **`top_up(capacity)`** réécrit :
  1. `slots = capacity - count_pending_batches()` ; si `≤0`, retour.
  2. `fetch_ready_batch_keys(slots)`.
  3. pour chaque clé : `transition_batch(key, Analyzed→PendingApproval, PendingApproval)` → si vide, `continue` ; sinon `fetch_batch_request_info` → `build_request` → `publish`.
- **`build_request(key, info)`** : assemble titre + agrégats (espace économisé total arrondi).
- **`run_stale_checker`** : `fetch_stale_pending_batches` → `build_request` + `publish` (pas de transition, comme aujourd'hui).
- **`handle_response(ApprovalResponse{batch_id, approved})`** : parse `batch_id` → `BatchKey` ; `transition_batch(key, PendingApproval → Transcoding|Skipped)` ; `wake.notify_one()`.
- **`wake_feeder()` / `run_approval_feeder`** : inchangés (le grain a changé, pas la mécanique de réveil).
- **Suppression** de l'ancien `send_request`/`resend_request` par fichier.

`workflow.rs` (bras `Analyzed → wake_feeder()`) : **inchangé**. La complétude est gérée par la requête de readiness, pas par le workflow.

## 5. Home Assistant (`homeassistant/rankoder-approval.yaml`)

Changements minimes :
- `media_file_id` → `batch_id` dans l'identifiant des boutons (`RANKODER_APPROVE|{{ batch_id }}`) et dans le payload de réponse (`{"batch_id": "...", "approved": ...}`).
- helper `rankoder_pending_id` → stocke le `batch_id` (string plus longue → passer `max:` à ~80).
- message : `"{{ file_count }} épisodes · {{ total_space_saved_gb }} Go économisés"`.
- `tag`, boutons, defer 9h–22h, replay 09:00 : identiques.

## Points de vigilance

- **Échos d'events** : `transition_batch` insère N events → N `pg_notify` → N `process_file` no-op (bras `PendingApproval`). Borné (K=1, une saison à la fois), acceptable ; on garde les events pour l'audit.
- **Épisode bloqué en `probed`** (probe qui traîne) : la saison n'est jamais « complète » → lot retardé. En pratique le probe se termine en `probed`/`failed` rapidement ; si tu veux blinder, on ajoutera plus tard un fallback « fenêtre de calme » au `HAVING`. Hors v1.
- **K unifié** : un film en attente consomme le même budget qu'une saison. Avec K=1, films et saisons s'enchaînent un par un — cohérent avec « valider une par une » sur HA. Si tu veux des budgets séparés (films vs séries), on en discutera, mais je garderais unifié.
- **Coût requête** : `fetch_ready_batch_keys` scanne `media_files⋈episodes` à chaque réveil. Avec `idx_media_files_active` + `idx_media_files_episode` c'est de l'ordre de la ms sur des milliers de lignes ; index dédié seulement si ça devient un point chaud.

---

Ordre d'implémentation suggéré : (1) `BatchKey`, (2) les 5 requêtes store, (3) messages MQTT, (4) module approval, (5) YAML HA, puis `cargo build` + `clippy`.

Je me lance sur cette base ?
