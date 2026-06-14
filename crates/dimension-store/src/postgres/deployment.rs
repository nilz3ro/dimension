//! DeploymentStore implementation for PgSessionStore.
//!
//! Provides CRUD and lifecycle operations for long-running deployment VMs.
//! The deployment_status column is a Postgres ENUM but is read as TEXT
//! to avoid a custom sqlx Decode implementation.

use async_trait::async_trait;
use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{Deployment, NewDeployment};
use crate::postgres::PgSessionStore;
use crate::store::DeploymentStore;

#[async_trait]
impl DeploymentStore for PgSessionStore {
    async fn create_deployment(&self, new: NewDeployment) -> Result<Deployment, StoreError> {
        self.create_deployment_impl(new).await
    }

    async fn get_deployment(&self, id: Uuid, user_id: Uuid) -> Result<Option<Deployment>, StoreError> {
        self.get_deployment_impl(id, user_id).await
    }

    async fn list_deployments(&self, user_id: Uuid) -> Result<Vec<Deployment>, StoreError> {
        self.list_deployments_impl(user_id).await
    }

    async fn update_deployment_status(&self, id: Uuid, status: &str) -> Result<(), StoreError> {
        self.update_deployment_status_impl(id, status).await
    }

    async fn update_deployment_worker(
        &self,
        id: Uuid,
        worker_id: &str,
        guest_ip: &str,
        pid: i32,
    ) -> Result<(), StoreError> {
        self.update_deployment_worker_impl(id, worker_id, guest_ip, pid).await
    }

    async fn increment_probe_failures(&self, id: Uuid) -> Result<i32, StoreError> {
        self.increment_probe_failures_impl(id).await
    }

    async fn reset_probe_failures(&self, id: Uuid) -> Result<(), StoreError> {
        self.reset_probe_failures_impl(id).await
    }

    async fn list_active_on_worker(&self, worker_id: &str) -> Result<Vec<Deployment>, StoreError> {
        self.list_active_on_worker_impl(worker_id).await
    }

    async fn mark_orphaned_for_worker(&self, worker_id: &str) -> Result<u64, StoreError> {
        self.mark_orphaned_for_worker_impl(worker_id).await
    }

    async fn get_deployment_public(&self, id: Uuid) -> Result<Option<Deployment>, StoreError> {
        self.get_deployment_public_impl(id).await
    }

    async fn list_active_worker_ids(&self) -> Result<Vec<String>, StoreError> {
        self.list_active_worker_ids_impl().await
    }

    async fn list_probeable_deployments(&self) -> Result<Vec<Deployment>, StoreError> {
        self.list_probeable_deployments_impl().await
    }
}

impl PgSessionStore {
    pub(crate) async fn create_deployment_impl(
        &self,
        new: NewDeployment,
    ) -> Result<Deployment, StoreError> {
        let deployment = query_as::<_, Deployment>(
            r#"
            INSERT INTO deployments (user_id, bundle_id, name, probe_port)
            VALUES ($1, $2, $3, $4)
            RETURNING
                id, user_id, bundle_id, name,
                status::TEXT AS status,
                worker_id, guest_ip, probe_port, pid, probe_failures,
                created_at, updated_at, stopped_at
            "#,
        )
        .bind(new.user_id)
        .bind(&new.bundle_id)
        .bind(&new.name)
        .bind(new.probe_port)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(deployment)
    }

    pub(crate) async fn get_deployment_impl(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Deployment>, StoreError> {
        let deployment = query_as::<_, Deployment>(
            r#"
            SELECT
                id, user_id, bundle_id, name,
                status::TEXT AS status,
                worker_id, guest_ip, probe_port, pid, probe_failures,
                created_at, updated_at, stopped_at
            FROM deployments
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(deployment)
    }

    pub(crate) async fn list_deployments_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<Deployment>, StoreError> {
        let deployments = query_as::<_, Deployment>(
            r#"
            SELECT
                id, user_id, bundle_id, name,
                status::TEXT AS status,
                worker_id, guest_ip, probe_port, pid, probe_failures,
                created_at, updated_at, stopped_at
            FROM deployments
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(deployments)
    }

    /// Update the status of a deployment.
    /// For 'stopped' and 'orphaned' statuses, also sets stopped_at = now().
    pub(crate) async fn update_deployment_status_impl(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<(), StoreError> {
        // Use a CASE expression to set stopped_at only for terminal statuses.
        query(
            r#"
            UPDATE deployments
            SET
                status = $1::deployment_status,
                updated_at = now(),
                stopped_at = CASE
                    WHEN $1 IN ('stopped', 'orphaned') THEN now()
                    ELSE stopped_at
                END
            WHERE id = $2
            "#,
        )
        .bind(status)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn update_deployment_worker_impl(
        &self,
        id: Uuid,
        worker_id: &str,
        guest_ip: &str,
        pid: i32,
    ) -> Result<(), StoreError> {
        query(
            r#"
            UPDATE deployments
            SET worker_id = $1, guest_ip = $2, pid = $3, updated_at = now()
            WHERE id = $4
            "#,
        )
        .bind(worker_id)
        .bind(guest_ip)
        .bind(pid)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn increment_probe_failures_impl(
        &self,
        id: Uuid,
    ) -> Result<i32, StoreError> {
        let row: (i32,) = sqlx_core::query_as::query_as(
            r#"
            UPDATE deployments
            SET probe_failures = probe_failures + 1, updated_at = now()
            WHERE id = $1
            RETURNING probe_failures
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(row.0)
    }

    pub(crate) async fn reset_probe_failures_impl(
        &self,
        id: Uuid,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE deployments SET probe_failures = 0, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn list_active_on_worker_impl(
        &self,
        worker_id: &str,
    ) -> Result<Vec<Deployment>, StoreError> {
        let deployments = query_as::<_, Deployment>(
            r#"
            SELECT
                id, user_id, bundle_id, name,
                status::TEXT AS status,
                worker_id, guest_ip, probe_port, pid, probe_failures,
                created_at, updated_at, stopped_at
            FROM deployments
            WHERE worker_id = $1
              AND status NOT IN ('stopped', 'orphaned')
            ORDER BY created_at DESC
            "#,
        )
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(deployments)
    }

    pub(crate) async fn mark_orphaned_for_worker_impl(
        &self,
        worker_id: &str,
    ) -> Result<u64, StoreError> {
        let result = query(
            r#"
            UPDATE deployments
            SET
                status = 'orphaned'::deployment_status,
                stopped_at = now(),
                updated_at = now()
            WHERE worker_id = $1
              AND status NOT IN ('stopped', 'orphaned')
            "#,
        )
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(result.rows_affected())
    }

    /// Get a deployment by ID without user scoping — used by the reverse proxy.
    pub(crate) async fn get_deployment_public_impl(
        &self,
        id: Uuid,
    ) -> Result<Option<Deployment>, StoreError> {
        let deployment = query_as::<_, Deployment>(
            r#"
            SELECT
                id, user_id, bundle_id, name,
                status::TEXT AS status,
                worker_id, guest_ip, probe_port, pid, probe_failures,
                created_at, updated_at, stopped_at
            FROM deployments
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(deployment)
    }

    /// List distinct worker_ids with active (non-stopped, non-orphaned) deployments.
    pub(crate) async fn list_active_worker_ids_impl(&self) -> Result<Vec<String>, StoreError> {
        let rows: Vec<(String,)> = sqlx_core::query_as::query_as(
            r#"
            SELECT DISTINCT worker_id
            FROM deployments
            WHERE worker_id IS NOT NULL
              AND status NOT IN ('stopped', 'orphaned')
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// List all deployments suitable for health probing.
    pub(crate) async fn list_probeable_deployments_impl(
        &self,
    ) -> Result<Vec<Deployment>, StoreError> {
        let deployments = query_as::<_, Deployment>(
            r#"
            SELECT
                id, user_id, bundle_id, name,
                status::TEXT AS status,
                worker_id, guest_ip, probe_port, pid, probe_failures,
                created_at, updated_at, stopped_at
            FROM deployments
            WHERE status IN ('health_checking', 'healthy')
              AND guest_ip IS NOT NULL
              AND stopped_at IS NULL
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(deployments)
    }
}
