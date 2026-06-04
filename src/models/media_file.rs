use uuid::Uuid;

use crate::impl_entity_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct MediaFileId(pub Uuid);

impl_entity_id!(MediaFileId);
