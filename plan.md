Tes choix se tiennent très bien, et ils simplifient même certains points. Mon évaluation rapide avant le plan :

- **Rétention + invisibilité Jellyfin/Radarr** : parfaitement faisable, mais ça impose **où** vivent les fichiers. La clé : l'original sort de l'arborescence scannée par Jellyfin/Radarr (déplacé vers un `retention_dir` *hors* bibliothèque), et le nouveau prend sa place. On ne peut pas juste renommer en `.bak` au même endroit, sinon Jellyfin verrait deux fichiers. ✅
- **10-bit (Main10)** : bon choix, meilleur rendu à taille égale, P4000 l'encode nativement. ✅
- **MKV + préservation des subs** : MKV est justement le seul conteneur qui garde *tous* les types de subs (y compris PGS/VOBSUB image) sans réencodage. On mappe tout en `copy`. ✅
- **Concurrence 1** : simplifie énormément (sémaphore = 1, un seul worker dédié). ✅

⚠️ Un point à connaître : **Radarr suit le fichier par son chemin**. Si on remplace le fichier dans son dos, Radarr le verra "manquant" jusqu'à un *Rescan*. Jellyfin, lui, re-scanne et retrouve l'item via le même `jellyfin_id`. Je prévois un webhook Radarr/Jellyfin optionnel en phase 2 (hors périmètre du cœur).

---

# Plan détaillé — implémentation du transcoding

## 1. Décisions figées
| Sujet       | Choix                                                                    |
| ----------- | ------------------------------------------------------------------------ |
| Sortie      | HEVC **Main10** (10-bit), conteneur **MKV**                              |
| Streams     | Vidéo réencodée, **audio + subs + chapitres + métadonnées copiés**       |
| Encoder     | Détection runtime : `nvenc > videotoolbox > libx265`, override env       |
| Concurrence | **1** fichier à la fois (sémaphore dédié, séparé du probe)               |
| Source      | Déplacée vers `retention_dir` hors bibliothèque, supprimée après N jours |
| Visibilité  | Jellyfin/Radarr ne voient que le nouveau fichier                         |

## 2. Nouveau module `src/transcode/` (calqué sur `probe/`)
```
transcode/
  mod.rs        # trait Transcoder + TranscodeOrchestrator (worker single-thread)
  encoder.rs    # enum Encoder, build_args(spec, source) -> Vec<String>
  detect.rs     # détection au démarrage (test-encode 1 frame), override env
  swap.rs       # déplacement atomique original->retention, temp->final
  progress.rs   # parse `-progress pipe:1` (phase 2 : publish MQTT)
  error.rs      # TranscodeError (thiserror)
```

**`detect.rs`** — au démarrage, teste réellement chaque encoder par ordre de priorité :
`ffmpeg -f lavfi -i testsrc=duration=0.1 -c:v hevc_nvenc -f null -` (exit 0 = dispo). On ne se fie **pas** à `ffmpeg -encoders` (`hevc_nvenc` y figure même quand CUDA échoue). Résultat mis en cache, surchargeable par `TRANSCODE_ENCODER`.

**`encoder.rs`** — `enum Encoder { Nvenc, VideoToolbox, Libx265 }`, `build_args` traduit le `crf` du spec :

- **Nvenc (P4000 / Pascal — pas de B-frames HEVC, sessions illimitées sur Quadro)**
  ```
  -map 0 -c copy -c:v hevc_nvenc -pix_fmt p010le -profile:v main10
  -preset p7 -tune hq -rc vbr -cq <crf> -b:v 0
  -spatial-aq 1 -aq-strength 8 -rc-lookahead 32 -bf 0 -tag:v hvc1
  ```
- **libx265 (fallback CPU)**
  ```
  -map 0 -c copy -c:v libx265 -pix_fmt yuv420p10le -profile:v main10
  -preset slow -crf <crf> -x265-params aq-mode=3 -tag:v hvc1
  ```
- **videotoolbox (Mac/dev)**
  ```
  -map 0 -c copy -c:v hevc_videotoolbox -pix_fmt p010le -profile:v main10
  -q:v <100 - crf*2> -tag:v hvc1
  ```
  (`-map 0 -c copy` puis override `-c:v` = vidéo réencodée, le reste copié.)

## 3. Orchestration & intégration
- **Nouveau `TranscodeOrchestrator`** avec sa propre `mpsc::Receiver<MediaFileId>` et une boucle worker unique (sémaphore `TRANSCODE_CONCURRENCY`, défaut 1). Même pattern que `WorkflowOrchestrator`/`PostgresListener`.
- **`WorkflowOrchestrator`** reçoit un `transcode_tx: mpsc::Sender<MediaFileId>`. Le bras `WorkflowStateTag::Transcoding` (aujourd'hui no-op, `workflow.rs:172`) fait simplement `transcode_tx.send(id)`. → on évite de bloquer un permit du pool workflow pendant tout l'encode.
- **Récupération au démarrage** (`main.rs`) : `store.fetch_files_in_state(Transcoding)` → ré-enqueue dans `transcode_tx` (reprise après crash), comme `fetch_active_media_files`.
- Câblage dans `main.rs` : `let (tx_t, rx_t) = mpsc::channel(100);`, spawn `transcode_orchestrator.run(token.child_token())`.

## 4. Déroulé d'un transcode (`TranscodeOrchestrator::transcode`)
1. Charge `MediaFile` (path + `video_properties` + `transcode_spec.crf`).
2. Encode vers `TRANSCODE_TMP_DIR/<media_file_id>.mkv` (hors bibliothèque → Jellyfin ne voit jamais de fichier partiel). Nom déterministe = leftover réutilisé après crash.
3. **Validation** : exit 0, ffprobe du résultat OK, durée ≈ source (tolérance ±1s/0.5%), codec = hevc.
4. **Seuil de gain** : si `new_size > original_size * (1 - TRANSCODE_MIN_SIZE_REDUCTION)` → on jette le temp, on **garde l'original intact**, transition `Transcoding → Skipped` (nouveau `SkipReason::InsufficientSizeReduction`).
5. **Swap** (`swap.rs`) — bibliothèque a toujours exactement un fichier :
   a. `rename(original → retention_dir/<id>__<filename>)`
   b. `rename(temp → <dossier original>/<basename>.mkv)`
   (rename atomique si même volume — recommander `TMP_DIR` et `retention_dir` sur le même FS que les médias ; sinon fallback copy+fsync+unlink.)
6. **DB** (transaction) : `UPDATE media_files SET file_path=<final>, size_bytes=<new>, video_codec='hevc', bitrate_kbps=<new>, workflow_state='done'` + `INSERT events (Transcoded{original_size,new_size})` + `INSERT retention_files`.
7. Échec à n'importe quelle étape avant le swap → temp supprimé, transition `Transcoding → Failed` + `TranscodeFailed{error}`. Original jamais touché.

**Recovery au démarrage** : pour chaque fichier en `Transcoding`, si `original_path` absent mais présent en retention → finir/annuler le swap selon ce qui existe (temp/final valide → terminer ; sinon restaurer l'original depuis retention) puis ré-encoder. Routine dédiée dans `swap.rs`.

## 5. Reaper de rétention
Tâche périodique (même forme que `run_stale_checker`, `approval/mod.rs:170`) : supprime les `retention_files` avec `moved_at < now() - TRANSCODE_RETENTION_DAYS`, `unlink` le fichier, supprime la ligne. Spawn dans `main.rs`.

## 6. Changements DB — `migrations/2_transcode.sql`
```sql
CREATE TABLE retention_files (
    id              UUID PRIMARY KEY,
    media_file_id   UUID NOT NULL REFERENCES media_files(id),
    retained_path   TEXT NOT NULL,
    original_size_bytes BIGINT NOT NULL,
    moved_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ON retention_files (moved_at);
```
- `transcode_spec` : enrichi à l'encode avec `{encoder, params}` pour l'audit (optionnel).
- États : pas de nouvel enum, on réutilise `transcoding/done/skipped/failed`.

## 7. Modèles à étendre
- `models/event.rs` : `MediaEvent::Transcoded { original_size, new_size }`, `TranscodeFailed { error }`.
- `models/workflow.rs` `next_on` : `(Transcoding, Transcoded) → Done`, `(Transcoding, TranscodeFailed) → Failed`, `(Transcoding, Skipped) → Skipped`.
- `models/transcode.rs` : `SkipReason::InsufficientSizeReduction`.
- `store/mod.rs` : `fetch_files_in_state`, `complete_transcode` (tx étape 6), `insert_retention_file`, `fetch_expired_retention_files`, `delete_retention_file`.

## 8. Config (`config.rs`)
| Env                            | Défaut | Rôle                                               |
| ------------------------------ | ------ | -------------------------------------------------- |
| `TRANSCODE_ENCODER`            | `auto` | auto / nvenc / videotoolbox / libx265              |
| `TRANSCODE_CONCURRENCY`        | `1`    | fichiers en parallèle                              |
| `TRANSCODE_TMP_DIR`            | requis | dossier de travail (même FS que médias idéalement) |
| `TRANSCODE_RETENTION_DIR`      | requis | dossier hors bibliothèque                          |
| `TRANSCODE_RETENTION_DAYS`     | `7`    | rétention avant suppression                        |
| `TRANSCODE_MIN_SIZE_REDUCTION` | `0.1`  | gain min (10 %) sinon on garde l'original          |
| `TRANSCODE_PRESET`             | (auto) | override preset par encoder                        |
| `TRANSCODE_DRY_RUN`            | `0`    | encode + valide mais ne swap pas                   |

## 9. Découpage en étapes (commits)
1. **Détection encoder** : `transcode/detect.rs` + `encoder.rs` (build_args), tests unitaires sur les args générés.
2. **Migration + modèles** : `2_transcode.sql`, events, `next_on`, `SkipReason`, méthodes store.
3. **Orchestrateur + exécution ffmpeg** : `transcode/mod.rs`, encode→temp, validation, branche `Transcoding` du workflow, câblage `main.rs` + recovery.
4. **Swap + rétention** : `swap.rs`, `complete_transcode`, reaper.
5. **Phase 2 (optionnel)** : progression via `-progress` + publish MQTT, webhook Radarr/Jellyfin rescan, dry-run.

## 10. Points ouverts / edge cases notés
- **Radarr** : "missing" jusqu'au rescan (webhook en phase 2).
- **TMP_DIR/RETENTION sur volume différent** des médias → pas de rename atomique, fallback copy (plus lent, fenêtre plus large). Recommandation : même FS.
- Sources déjà 10-bit / HDR : on garde le pix_fmt source (pas de tone-mapping). À vérifier qu'on ne casse pas le HDR (copie des métadonnées de couleur `-color_*` côté nvenc).
- `crf→cq` : équivalent en première approche ; éventuel `-1` car le 10-bit est plus efficace (à calibrer sur tes fichiers).

