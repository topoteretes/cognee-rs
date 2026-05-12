use sea_orm_migration::prelude::*;

mod m20250101_000001_initial_schema;
mod m20250201_000001_acl_tables;
mod m20250301_000001_add_importance_weight;
mod m20250422_000001_user_tenant_role_tables;
mod m20260424_000001_graph_sync_checkpoints;
mod m20260427_000001_http_auth_columns;
mod m20260428_000001_tenants_rbac;
mod m20260429_000001_sync_operations;
mod m20260501_000001_create_notebooks;
mod m20260501_000002_pipeline_run_payload_fields;
mod m20260501_000003_session_records;
mod m20260512_000001_add_parent_user_id;
mod m20260901_000003_pipeline_run_dataset_nullable;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250101_000001_initial_schema::Migration),
            Box::new(m20250201_000001_acl_tables::Migration),
            Box::new(m20250301_000001_add_importance_weight::Migration),
            Box::new(m20250422_000001_user_tenant_role_tables::Migration),
            Box::new(m20260424_000001_graph_sync_checkpoints::Migration),
            Box::new(m20260427_000001_http_auth_columns::Migration),
            Box::new(m20260428_000001_tenants_rbac::Migration),
            Box::new(m20260429_000001_sync_operations::Migration),
            Box::new(m20260501_000001_create_notebooks::Migration),
            Box::new(m20260501_000002_pipeline_run_payload_fields::Migration),
            Box::new(m20260501_000003_session_records::Migration),
            Box::new(m20260512_000001_add_parent_user_id::Migration),
            Box::new(m20260901_000003_pipeline_run_dataset_nullable::Migration),
        ]
    }
}
