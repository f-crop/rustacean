#![allow(clippy::missing_errors_doc)]

mod error;
mod kafka;
mod pg;

pub use error::MigrateError;
pub use kafka::{
    ApplyResult, KafkaAdmin, TopicDef, TopicStatus, TopicsFile, apply_topics, load_topics_file,
    print_status,
};
pub use pg::{
    control_status, migrate_all_tenants, migrate_control, migrate_tenant, migrate_tenant_schema,
    tenant_schemas, tenant_status,
};
