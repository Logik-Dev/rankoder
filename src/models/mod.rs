pub mod common;
pub mod drafts;
pub mod episode;
pub mod error;
pub mod event;
pub mod media_file;
pub mod movie;
pub mod series;
pub mod transcode;
pub mod video;
pub mod workflow;

#[macro_export]
macro_rules! impl_entity_id {
    ($name:ident) => {
        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::now_v7())
            }

            pub fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<uuid::Uuid> for $name {
            fn from(uuid: uuid::Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}
