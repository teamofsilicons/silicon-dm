//! Durable mutation plans and encrypted replay responses.

use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, pool::PoolConnection};
use uuid::Uuid;

use super::{TestingRegistry, new_root_key};
use crate::{AppError, AppResult, application::auth::AuthContext};

pub(super) struct Mutation {
    // Session advisory lock held for the entire cross-database operation. Closing
    // rather than pooling this connection releases the lock even on cancellation.
    _lease: PoolConnection<Postgres>,
    pub id: Uuid,
    pub environment_id: Uuid,
    pub key: SecretString,
    pub replay: Option<Value>,
}

impl TestingRegistry {
    pub(super) async fn begin_mutation(
        &self,
        auth: &AuthContext,
        operation: &str,
        key: &str,
        input: &Value,
        environment_id: Option<Uuid>,
    ) -> AppResult<Mutation> {
        self.begin_scoped_mutation(
            (
                auth.organization_id.as_str(),
                auth.actor.actor_type.as_str(),
                auth.actor.id.as_str(),
            ),
            operation,
            key,
            input,
            environment_id,
        )
        .await
    }

    pub(super) async fn begin_scoped_mutation(
        &self,
        (organization_id, actor_kind, actor_id): (&str, &str, &str),
        operation: &str,
        key: &str,
        input: &Value,
        environment_id: Option<Uuid>,
    ) -> AppResult<Mutation> {
        let scope = serde_json::to_vec(&(organization_id, actor_kind, actor_id, operation, key))
            .map_err(AppError::internal)?;
        let scope_hash = blake3::hash(&scope);
        let lock = i64::from_be_bytes(
            scope_hash.as_bytes()[..8]
                .try_into()
                .map_err(AppError::internal)?,
        );
        let mut lease = self.production.pool().acquire().await?;
        lease.close_on_drop();
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(lock)
            .execute(&mut *lease)
            .await?;
        let request_hash = blake3::hash(&serde_json::to_vec(input).map_err(AppError::internal)?);
        let id = Uuid::now_v7();
        let planned_key = new_root_key();
        sqlx::query("INSERT INTO dm.testing_mutations (mutation_id,organization_id,actor_kind,actor_id,operation,idempotency_key,request_hash,environment_id,planned_key_ciphertext) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(organization_id,actor_kind,actor_id,operation,idempotency_key) DO NOTHING")
            .bind(id).bind(organization_id).bind(actor_kind).bind(actor_id).bind(operation).bind(key)
            .bind(request_hash.as_bytes().as_slice()).bind(environment_id.unwrap_or_else(Uuid::now_v7)).bind(self.encrypt(id,"mutation-key",&planned_key)?)
            .execute(&mut *lease).await?;
        let (id, stored_hash, selected_id, encrypted_key, response):(Uuid,Vec<u8>,Uuid,String,Option<String>)=sqlx::query_as("SELECT mutation_id,request_hash,environment_id,planned_key_ciphertext,response_ciphertext FROM dm.testing_mutations WHERE organization_id=$1 AND actor_kind=$2 AND actor_id=$3 AND operation=$4 AND idempotency_key=$5")
            .bind(organization_id).bind(actor_kind).bind(actor_id).bind(operation).bind(key).fetch_one(&mut *lease).await?;
        if stored_hash != request_hash.as_bytes().as_slice()
            || environment_id.is_some_and(|expected| expected != selected_id)
        {
            return Err(AppError::conflict(
                "idempotency key was already used with a different request",
            ));
        }
        let replay = response
            .map(|encrypted| {
                let response = self.decrypt(id, "mutation-response", &encrypted)?;
                serde_json::from_str(response.expose_secret()).map_err(AppError::internal)
            })
            .transpose()?;
        Ok(Mutation {
            _lease: lease,
            id,
            environment_id: selected_id,
            key: self.decrypt(id, "mutation-key", &encrypted_key)?,
            replay,
        })
    }

    pub(super) async fn complete_mutation(
        &self,
        connection: &mut PgConnection,
        mutation: &Mutation,
        response: &Value,
    ) -> AppResult<()> {
        let serialized =
            SecretString::from(serde_json::to_string(response).map_err(AppError::internal)?);
        sqlx::query("UPDATE dm.testing_mutations SET response_ciphertext=$2,completed_at=clock_timestamp() WHERE mutation_id=$1 AND response_ciphertext IS NULL")
            .bind(mutation.id).bind(self.encrypt(mutation.id,"mutation-response",&serialized)?).execute(connection).await?;
        Ok(())
    }
}
