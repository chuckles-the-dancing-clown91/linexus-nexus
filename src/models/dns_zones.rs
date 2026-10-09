//! BIND zones and their records (Nexus is the source of truth for them).

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, PaginatorTrait, QueryOrder};
use uuid::Uuid;

pub use super::_entities::dns_records::{
    self, ActiveModel as RecordActiveModel, Entity as RecordEntity, Model as Record,
};
pub use super::_entities::dns_zones::{self, ActiveModel, Entity, Model};

/// The next SOA serial in `YYYYMMDDnn` form: today's first serial, or one
/// more than `current` when that is already today's (or later).
#[must_use]
pub fn next_serial(current: i64, today: chrono::NaiveDate) -> i64 {
    let base = i64::from(
        today
            .format("%Y%m%d")
            .to_string()
            .parse::<i32>()
            .unwrap_or(0),
    ) * 100;
    if current >= base {
        current + 1
    } else {
        base + 1
    }
}

impl Model {
    pub async fn list(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(Entity::find()
            .order_by_asc(dns_zones::Column::Name)
            .all(db)
            .await?)
    }

    pub async fn find_by_zone_id(db: &DatabaseConnection, id: &Uuid) -> ModelResult<Self> {
        Entity::find()
            .filter(dns_zones::Column::ZoneId.eq(*id))
            .one(db)
            .await?
            .ok_or(ModelError::EntityNotFound)
    }

    pub async fn find_by_name(db: &DatabaseConnection, name: &str) -> ModelResult<Option<Self>> {
        Ok(Entity::find()
            .filter(dns_zones::Column::Name.eq(name))
            .one(db)
            .await?)
    }

    #[must_use]
    pub fn secondaries(&self) -> Vec<Uuid> {
        self.secondary_agent_ids
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect()
    }

    #[must_use]
    pub fn task_list(&self) -> Vec<String> {
        self.task_ids
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }

    /// How many records the zone holds.
    pub async fn record_count(&self, db: &DatabaseConnection) -> ModelResult<u64> {
        Ok(RecordEntity::find()
            .filter(dns_records::Column::ZoneId.eq(self.zone_id))
            .count(db)
            .await?)
    }

    pub async fn records(&self, db: &DatabaseConnection) -> ModelResult<Vec<Record>> {
        Ok(RecordEntity::find()
            .filter(dns_records::Column::ZoneId.eq(self.zone_id))
            .order_by_asc(dns_records::Column::Name)
            .order_by_asc(dns_records::Column::RecordType)
            .order_by_asc(dns_records::Column::Id)
            .all(db)
            .await?)
    }

    pub async fn find_record(
        &self,
        db: &DatabaseConnection,
        record_id: &Uuid,
    ) -> ModelResult<Record> {
        RecordEntity::find()
            .filter(dns_records::Column::ZoneId.eq(self.zone_id))
            .filter(dns_records::Column::RecordId.eq(*record_id))
            .one(db)
            .await?
            .ok_or(ModelError::EntityNotFound)
    }

    /// Bump the serial (see [`next_serial`]).
    pub async fn bump_serial(self, db: &DatabaseConnection) -> ModelResult<Self> {
        let serial = next_serial(self.serial, chrono::Utc::now().date_naive());
        let mut active: ActiveModel = self.into();
        active.serial = ActiveValue::set(serial);
        active.updated_at = ActiveValue::set(chrono::Utc::now().into());
        Ok(active.update(db).await?)
    }

    /// Remember the tasks dispatched for the latest change.
    pub async fn set_tasks(
        self,
        db: &DatabaseConnection,
        task_ids: &[String],
    ) -> ModelResult<Self> {
        let mut active: ActiveModel = self.into();
        active.last_task_id = ActiveValue::set(task_ids.first().cloned());
        active.task_ids =
            ActiveValue::set(Some(serde_json::to_string(task_ids).unwrap_or_default()));
        Ok(active.update(db).await?)
    }

    /// Delete the zone and its records.
    pub async fn delete_with_records(self, db: &DatabaseConnection) -> ModelResult<()> {
        RecordEntity::delete_many()
            .filter(dns_records::Column::ZoneId.eq(self.zone_id))
            .exec(db)
            .await?;
        Entity::delete_by_id(self.id).exec(db).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_rolls_per_day() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        assert_eq!(next_serial(0, d), 2_026_100_901);
        assert_eq!(next_serial(2_026_100_901, d), 2_026_100_902);
        assert_eq!(next_serial(2_026_100_899, d), 2_026_100_901);
        // A serial already ahead of today (clock went back) still increases.
        assert_eq!(next_serial(2_026_101_005, d), 2_026_101_006);
    }
}
