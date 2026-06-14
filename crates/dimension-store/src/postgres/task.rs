//! TaskStore implementation for PgSessionStore.
//!
//! Implements all task/run/target CRUD operations plus the scheduler helper.

use async_trait::async_trait;
use sqlx_core::query::query;
use sqlx_core::row::Row;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{NewTask, NewTaskRun, Task, TaskRun};
use crate::postgres::PgSessionStore;
use crate::store::{SessionStore, TaskStore};

// ── Internal row helpers ──────────────────────────────────────────────────────

fn task_from_row(row: &sqlx_postgres::PgRow) -> Result<Task, sqlx_core::Error> {
    Ok(Task {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        bundle_id: row.try_get("bundle_id")?,
        goal: row.try_get("goal")?,
        success_criteria: row.try_get("success_criteria")?,
        evaluator_bundle_id: row.try_get("evaluator_bundle_id")?,
        trigger_type: row.try_get("trigger_type")?,
        cron_expr: row.try_get("cron_expr")?,
        next_run_at: row.try_get("next_run_at")?,
        max_iterations: row.try_get("max_iterations")?,
        timeout_hours: row.try_get("timeout_hours")?,
        iteration_count: row.try_get("iteration_count")?,
        status: row.try_get("status")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
        last_output: row.try_get("last_output")?,
        last_eval_feedback: row.try_get("last_eval_feedback")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn task_run_from_row(row: &sqlx_postgres::PgRow) -> Result<TaskRun, sqlx_core::Error> {
    Ok(TaskRun {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        iteration: row.try_get("iteration")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
        output: row.try_get("output")?,
        eval_feedback: row.try_get("eval_feedback")?,
        eval_passed: row.try_get("eval_passed")?,
        error: row.try_get("error")?,
        created_at: row.try_get("created_at")?,
    })
}

// ── TaskStore implementation ──────────────────────────────────────────────────

#[async_trait]
impl TaskStore for PgSessionStore {
    async fn upsert_agent_card(
        &self,
        bundle_id: &str,
        user_id: Uuid,
        card_json: serde_json::Value,
    ) -> Result<(), StoreError> {
        self.upsert_agent_card_impl(bundle_id, user_id, card_json).await
    }

    async fn get_agent_card(
        &self,
        bundle_id: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        self.get_agent_card_impl(bundle_id).await
    }

    async fn delete_agent_card(&self, bundle_id: &str) -> Result<(), StoreError> {
        self.delete_agent_card_impl(bundle_id).await
    }

    async fn get_or_create_agent_session(
        &self,
        caller_bundle_id: &str,
        target_bundle_id: &str,
        user_id: Uuid,
        session_store: &dyn SessionStore,
    ) -> Result<Uuid, StoreError> {
        self.get_or_create_agent_session_impl(
            caller_bundle_id,
            target_bundle_id,
            user_id,
            session_store,
        )
        .await
    }

    async fn create_task(&self, task: NewTask) -> Result<Task, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from)?;

        let row = sqlx_core::query::query(
            "INSERT INTO tasks (
                user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             RETURNING id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                       trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                       iteration_count, status, started_at, completed_at, last_output,
                       last_eval_feedback, created_at, updated_at",
        )
        .bind(task.user_id)
        .bind(&task.bundle_id)
        .bind(&task.goal)
        .bind(&task.success_criteria)
        .bind(&task.evaluator_bundle_id)
        .bind(&task.trigger_type)
        .bind(&task.cron_expr)
        .bind(task.next_run_at)
        .bind(task.max_iterations)
        .bind(task.timeout_hours)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::from)?;

        let created_task = task_from_row(&row).map_err(StoreError::from)?;

        // Insert fan-out targets.
        for bundle_id in &task.target_bundle_ids {
            sqlx_core::query::query(
                "INSERT INTO task_targets (task_id, bundle_id) VALUES ($1, $2)",
            )
            .bind(created_task.id)
            .bind(bundle_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::from)?;
        }

        tx.commit().await.map_err(StoreError::from)?;

        Ok(created_task)
    }

    async fn get_task(&self, task_id: Uuid, user_id: Uuid) -> Result<Option<Task>, StoreError> {
        let row = query(
            "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                    trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                    iteration_count, status, started_at, completed_at, last_output,
                    last_eval_feedback, created_at, updated_at
             FROM tasks
             WHERE id = $1 AND user_id = $2",
        )
        .bind(task_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            None => Ok(None),
            Some(r) => Ok(Some(task_from_row(&r).map_err(StoreError::from)?)),
        }
    }

    async fn get_task_scoped(
        &self,
        task_id: Uuid,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Option<Task>, StoreError> {
        let row = query(
            "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                    trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                    iteration_count, status, started_at, completed_at, last_output,
                    last_eval_feedback, created_at, updated_at
             FROM tasks
             WHERE id = $1 AND user_id = $2 AND bundle_id = $3",
        )
        .bind(task_id)
        .bind(user_id)
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            None => Ok(None),
            Some(r) => Ok(Some(task_from_row(&r).map_err(StoreError::from)?)),
        }
    }

    async fn list_tasks(
        &self,
        user_id: Uuid,
        status: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Task>, Option<String>), StoreError> {
        let cursor_ts: Option<chrono::DateTime<chrono::Utc>> = cursor
            .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let rows = if let Some(status_filter) = status {
            if let Some(ts) = cursor_ts {
                query(
                    "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                            trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                            iteration_count, status, started_at, completed_at, last_output,
                            last_eval_feedback, created_at, updated_at
                     FROM tasks
                     WHERE user_id = $1 AND status = $2 AND created_at <= $3
                     ORDER BY created_at DESC
                     LIMIT $4",
                )
                .bind(user_id)
                .bind(status_filter)
                .bind(ts)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await
                .map_err(StoreError::from)?
            } else {
                query(
                    "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                            trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                            iteration_count, status, started_at, completed_at, last_output,
                            last_eval_feedback, created_at, updated_at
                     FROM tasks
                     WHERE user_id = $1 AND status = $2
                     ORDER BY created_at DESC
                     LIMIT $3",
                )
                .bind(user_id)
                .bind(status_filter)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await
                .map_err(StoreError::from)?
            }
        } else if let Some(ts) = cursor_ts {
            query(
                "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                        trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                        iteration_count, status, started_at, completed_at, last_output,
                        last_eval_feedback, created_at, updated_at
                 FROM tasks
                 WHERE user_id = $1 AND created_at <= $2
                 ORDER BY created_at DESC
                 LIMIT $3",
            )
            .bind(user_id)
            .bind(ts)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?
        } else {
            query(
                "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                        trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                        iteration_count, status, started_at, completed_at, last_output,
                        last_eval_feedback, created_at, updated_at
                 FROM tasks
                 WHERE user_id = $1
                 ORDER BY created_at DESC
                 LIMIT $2",
            )
            .bind(user_id)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?
        };

        let has_next = rows.len() as i64 > limit;
        let tasks: Vec<Task> = rows
            .iter()
            .take(limit as usize)
            .map(|r| task_from_row(r).map_err(StoreError::from))
            .collect::<Result<Vec<_>, _>>()?;

        let next_cursor = if has_next {
            tasks.last().map(|t| t.created_at.to_rfc3339())
        } else {
            None
        };

        Ok((tasks, next_cursor))
    }

    async fn update_task_status(&self, task_id: Uuid, status: &str) -> Result<(), StoreError> {
        query(
            "UPDATE tasks SET status = $2, updated_at = NOW() WHERE id = $1",
        )
        .bind(task_id)
        .bind(status)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    async fn cancel_task(&self, task_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        query(
            "UPDATE tasks
             SET status = 'cancelled', updated_at = NOW()
             WHERE id = $1 AND user_id = $2 AND status IN ('pending', 'running')",
        )
        .bind(task_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    async fn claim_ready_tasks(&self, limit: i32) -> Result<Vec<Task>, StoreError> {
        // CRITICAL: Single-statement atomic claim with UPDATE ... WHERE id IN (SELECT ... FOR UPDATE SKIP LOCKED)
        // This pattern ensures:
        // 1. The FOR UPDATE SKIP LOCKED prevents two workers from selecting the same task.
        // 2. The UPDATE atomically transitions status to 'running' in the same statement.
        // 3. RETURNING * gives us the updated rows without a second query.
        let rows = query(
            "UPDATE tasks
             SET status = 'running',
                 started_at = COALESCE(started_at, NOW()),
                 updated_at = NOW()
             WHERE id IN (
                 SELECT id FROM tasks
                 WHERE status = 'pending'
                   AND (next_run_at IS NULL OR next_run_at <= NOW())
                 ORDER BY next_run_at ASC NULLS FIRST
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                       trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                       iteration_count, status, started_at, completed_at, last_output,
                       last_eval_feedback, created_at, updated_at",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let tasks: Vec<Task> = rows
            .iter()
            .map(|r| task_from_row(r).map_err(StoreError::from))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    async fn complete_task_iteration(
        &self,
        task_id: Uuid,
        run: NewTaskRun,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from)?;

        // Insert the task run record.
        sqlx_core::query::query(
            "INSERT INTO task_runs (task_id, iteration, completed_at, output, eval_feedback, eval_passed, error)
             VALUES ($1, $2, NOW(), $3, $4, $5, $6)",
        )
        .bind(run.task_id)
        .bind(run.iteration)
        .bind(&run.output)
        .bind(&run.eval_feedback)
        .bind(run.eval_passed)
        .bind(&run.error)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;

        // Update the task: increment iteration_count, set last_output/feedback.
        sqlx_core::query::query(
            "UPDATE tasks
             SET iteration_count = iteration_count + 1,
                 last_output = $2,
                 last_eval_feedback = $3,
                 updated_at = NOW()
             WHERE id = $1",
        )
        .bind(task_id)
        .bind(&run.output)
        .bind(&run.eval_feedback)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;

        tx.commit().await.map_err(StoreError::from)?;

        Ok(())
    }

    async fn get_task_runs(
        &self,
        task_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<TaskRun>, StoreError> {
        // Verify task belongs to user first.
        let exists = query("SELECT 1 FROM tasks WHERE id = $1 AND user_id = $2")
            .bind(task_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::from)?;

        if exists.is_none() {
            return Ok(vec![]);
        }

        let rows = query(
            "SELECT id, task_id, iteration, started_at, completed_at,
                    output, eval_feedback, eval_passed, error, created_at
             FROM task_runs
             WHERE task_id = $1
             ORDER BY iteration ASC",
        )
        .bind(task_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let runs: Vec<TaskRun> = rows
            .iter()
            .map(|r| task_run_from_row(r).map_err(StoreError::from))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(runs)
    }

    async fn count_running_tasks(&self, user_id: Uuid) -> Result<i64, StoreError> {
        let row = query(
            "SELECT COUNT(*) as cnt FROM tasks WHERE user_id = $1 AND status = 'running'",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let cnt: i64 = row.try_get("cnt").map_err(StoreError::from)?;
        Ok(cnt)
    }

    async fn get_task_targets(&self, task_id: Uuid) -> Result<Vec<String>, StoreError> {
        let rows = query("SELECT bundle_id FROM task_targets WHERE task_id = $1")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?;

        let bundle_ids: Vec<String> = rows
            .iter()
            .map(|r| r.try_get::<String, _>("bundle_id").map_err(StoreError::from))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(bundle_ids)
    }

    async fn set_task_next_run(
        &self,
        task_id: Uuid,
        next_run_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE tasks SET next_run_at = $2, status = 'pending', updated_at = NOW() WHERE id = $1",
        )
        .bind(task_id)
        .bind(next_run_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    async fn admin_list_tasks(
        &self,
        status: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Task>, Option<String>), StoreError> {
        let cursor_ts: Option<chrono::DateTime<chrono::Utc>> = cursor
            .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let rows = match (status, cursor_ts) {
            (Some(s), Some(ts)) => query(
                "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                        trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                        iteration_count, status, started_at, completed_at, last_output,
                        last_eval_feedback, created_at, updated_at
                 FROM tasks
                 WHERE status = $1 AND created_at <= $2
                 ORDER BY created_at DESC
                 LIMIT $3",
            )
            .bind(s)
            .bind(ts)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?,
            (Some(s), None) => query(
                "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                        trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                        iteration_count, status, started_at, completed_at, last_output,
                        last_eval_feedback, created_at, updated_at
                 FROM tasks
                 WHERE status = $1
                 ORDER BY created_at DESC
                 LIMIT $2",
            )
            .bind(s)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?,
            (None, Some(ts)) => query(
                "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                        trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                        iteration_count, status, started_at, completed_at, last_output,
                        last_eval_feedback, created_at, updated_at
                 FROM tasks
                 WHERE created_at <= $1
                 ORDER BY created_at DESC
                 LIMIT $2",
            )
            .bind(ts)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?,
            (None, None) => query(
                "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                        trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                        iteration_count, status, started_at, completed_at, last_output,
                        last_eval_feedback, created_at, updated_at
                 FROM tasks
                 ORDER BY created_at DESC
                 LIMIT $1",
            )
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?,
        };

        let has_next = rows.len() as i64 > limit;
        let tasks: Vec<Task> = rows
            .iter()
            .take(limit as usize)
            .map(|r| task_from_row(r).map_err(StoreError::from))
            .collect::<Result<Vec<_>, _>>()?;

        let next_cursor = if has_next {
            tasks.last().map(|t| t.created_at.to_rfc3339())
        } else {
            None
        };

        Ok((tasks, next_cursor))
    }

    async fn admin_get_task(&self, task_id: Uuid) -> Result<Option<Task>, StoreError> {
        let row = query(
            "SELECT id, user_id, bundle_id, goal, success_criteria, evaluator_bundle_id,
                    trigger_type, cron_expr, next_run_at, max_iterations, timeout_hours,
                    iteration_count, status, started_at, completed_at, last_output,
                    last_eval_feedback, created_at, updated_at
             FROM tasks
             WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            None => Ok(None),
            Some(r) => Ok(Some(task_from_row(&r).map_err(StoreError::from)?)),
        }
    }

    async fn retry_task(&self, task_id: Uuid) -> Result<(), StoreError> {
        let result = query(
            "UPDATE tasks
             SET status = 'pending',
                 iteration_count = 0,
                 started_at = NULL,
                 completed_at = NULL,
                 updated_at = NOW()
             WHERE id = $1 AND status IN ('failed', 'cancelled')",
        )
        .bind(task_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::Other(
                "task not found or not in a retryable state (must be failed or cancelled)".into(),
            ));
        }

        Ok(())
    }
}
