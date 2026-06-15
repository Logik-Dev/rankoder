Excellente question — et la réponse « pro » est un peu à contre-courant de ta proposition : **ne change pas de lib.** Tu as déjà le bon outillage et il est cohérent dans tout le projet :

- `thiserror` pour les erreurs typées par module (domaine/lib) — `StoreError`, `ProbeError`, `DomainError`…
- `anyhow` à la frontière applicative (les tâches/orchestrateurs).

C'est exactement la convention idiomatique Rust (typed errors pour les bibliothèques, `anyhow` pour le binaire). Passer à `snafu`, `eyre` ou `error-stack` ajouterait de la dette de cohérence pour un gain quasi nul ici. **Le problème n'est pas la lib, il est architectural.** Être pro, c'est résister à l'ajout de dépendance et corriger le design.

Le vrai défaut actuel : `process_file` **mélange deux responsabilités** — la logique qui échoue, et l'effet de bord qui enregistre l'échec (`store.transition(...)` répété 5 fois). D'où la fragilité (oublier une transition, se tromper de `from`-state) et les variantes mortes.

## Les 3 corrections de fond

### 1. Distinguer *outcome* (résultat métier) et *error* (échec)
`InsufficientSizeReduction` n'est **pas une erreur** — c'est un résultat normal du workflow (→ `Skipped`). C'est pour ça qu'elle traîne, mal construite, dans `TranscodeError`. Sépare les deux :

```rust
// transcode/outcome.rs
pub enum TranscodeOutcome {
    Completed(CompletedTranscode),  // -> complete_transcode (Done)
    Skipped(SkipReason),            // -> Skipped (réduction insuffisante)
    AlreadyRecovered,               // recovery a déjà commité, rien à faire
}

pub struct CompletedTranscode {
    pub final_path: AbsoluteFilePath,
    pub new_size: SizeBytes,
    pub bitrate: Option<Bitrate>,
    pub retention_path: String,
}
```
`TranscodeError` ne garde que les vrais échecs, et on **construit enfin les variantes typées** (`FfmpegFailed { exit_code, stderr }` au lieu d'un `format!` ad-hoc) → fin des warnings dead-code.

### 2. Classer les erreurs : terminal vs transient
Tout `Err` ne doit pas devenir `Failed`. Un échec ffmpeg/validation/swap est **terminal** (→ `Failed`). Une panne DB pendant le commit est **transitoire** (→ laisser en `Transcoding`, rejouer au prochain boot, c'est déjà ce que fait ta recovery). On encode ça sur le type :

```rust
impl TranscodeError {
    /// Terminal -> enregistré en `Failed`. Transient -> propagé, l'état reste
    /// `Transcoding` pour être rejoué.
    fn is_terminal(&self) -> bool {
        matches!(self,
            Self::FfmpegFailed { .. } | Self::Validation(_)
            | Self::SwapFailed(_) | Self::MissingSpec | Self::Domain(_))
    }
}
```

### 3. Centraliser l'effet en *un seul* point (railway-oriented)
La logique pure renvoie `Result<TranscodeOutcome, TranscodeError>` (avec `?` partout, plus aucun `store.transition` au milieu). Un dispatcher fait la traduction unique :

```rust
async fn process_file(store, ..., id) -> anyhow::Result<()> {
    match Self::run(store.clone(), ..., id).await {
        Ok(Completed(c))      => store.complete_transcode(&id, &c.final_path, c.new_size,
                                      c.bitrate.as_ref(), original_size, &c.retention_path).await?,
        Ok(Skipped(reason))   => store.apply_event(&id, Transcoding,
                                      &MediaEvent::Skipped { reason, bpp: None, compression_potential: None }).await?,
        Ok(AlreadyRecovered)  => {}
        Err(e) if e.is_terminal() => store.apply_event(&id, Transcoding,
                                      &MediaEvent::TranscodeFailed { error: e.to_string() }).await?,
        Err(e) => error!(%e, ?id, "transient transcode error, left in Transcoding for retry"),
    }
    Ok(())
}
```

## Le bonus de cohérence : exploiter ta machine à états existante
Tu as déjà `WorkflowStateTag::next_on(event)` (`workflow.rs`). Aujourd'hui les appelants passent le `to`-state **à la main**, ce qui peut diverger de la machine. Ajoute un seul helper au store et le `to` devient dérivé, impossible à désynchroniser :

```rust
pub async fn apply_event(&self, id: &MediaFileId, from: WorkflowStateTag, event: &MediaEvent)
    -> Result<(), StoreError> {
    let to = from.next_on(event).ok_or(StoreError::InvalidTransition { from })?;
    self.transition(id, from, to, event).await
}
```
Gain transverse : `analyze`/`approval` pourront s'en servir plus tard → une seule source de vérité pour les transitions.

## Polish RAII (optionnel mais très pro)
Le `let _ = tokio::fs::remove_file(&temp_path)` répété 3× → un garde qui nettoie au `Drop`, désarmé après swap réussi :
```rust
struct ScopedTemp(PathBuf);
impl Drop for ScopedTemp { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
// .disarm() / std::mem::forget après le swap OK
```

---

**Résumé** : pas de nouvelle lib. Tu sépares *outcome*/*error*, tu classes terminal/transient, tu centralises la traduction en un point, et tu t'appuies sur `next_on` via `apply_event`. Résultat : `process_file` passe de ~5 blocs dupliqués à un `match` linéaire, les variantes typées sont enfin utilisées (warnings réglés), et les transitions ne peuvent plus diverger de la machine à états.

Tu veux que je l'implémente ? Je propose l'ordre : `apply_event` au store → `TranscodeOutcome` + nettoyage de `TranscodeError` → refacto `process_file` en `run` + dispatcher → garde RAII. Je peux tout faire en un commit cohérent ou en 4 commits séparés, comme tu préfères.
