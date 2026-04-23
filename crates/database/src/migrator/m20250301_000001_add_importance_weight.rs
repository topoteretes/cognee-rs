use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Data::Table)
                    .add_column(ColumnDef::new(Data::ImportanceWeight).double().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Data::Table)
                    .drop_column(Data::ImportanceWeight)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Data {
    Table,
    ImportanceWeight,
}
