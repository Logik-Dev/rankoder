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
