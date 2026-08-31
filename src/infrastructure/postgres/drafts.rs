//! Optimistically versioned draft persistence.

use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::{PutDraftCommand, PutDraftOutcome},
    domain::{ActorRef, Attachment, Draft, Gif, OrganizationId},
};

use super::{PostgresStore, map_constraint_error, rows::parse_url};

#[derive(Debug, FromRow)]
struct DraftRecord {
    conversation_id: Uuid,
    actor_id: String,
    version: i64,
    text_content: Option<String>,
    updated_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct DraftAttachmentRecord {
    attachment_kind: String,
    permanent_url: String,
    name: Option<String>,
    content_type: Option<String>,
    declared_size_bytes: Option<i64>,
}

#[derive(Debug, FromRow)]
struct DraftGifRecord {
    provider_id: String,
    url: String,
    preview_url: Option<String>,
    title: Option<String>,
}

impl PostgresStore {
    /// Returns the authenticated participant's current draft.
    ///
    /// # Errors
    ///
    /// Returns not-found when the caller is not a participant or has no draft,
    /// and reports malformed durable data as an internal error.
    pub async fn get_draft(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<Draft> {
        self.require_participant(organization_id, actor, conversation_id)
            .await?;
        self.load_draft(organization_id, actor, conversation_id)
            .await
    }

    /// Creates or replaces a synchronized draft using exact-version compare-and-swap.
    ///
    /// # Errors
    ///
    /// Rejects invalid content, a non-participant, or a stale expected version.
    #[allow(
        clippy::too_many_lines,
        reason = "draft CAS, version-token mutation, children, and returned snapshot must remain one auditable transaction"
    )]
    pub async fn put_draft(&self, command: PutDraftCommand) -> AppResult<PutDraftOutcome> {
        command.input.validate().map_err(AppError::validation)?;
        let content_hash = command.input.content_digest();
        let mut transaction = self.pool().begin().await?;
        let participant_exists = sqlx::query_scalar::<_, i32>(
            r#"
            SELECT 1
            FROM dm.conversation_participants
            WHERE conversation_id = $1
              AND organization_id = $2
              AND actor_kind = $3::text::dm.actor_kind
              AND actor_id = $4
            FOR UPDATE
            "#,
        )
        .bind(command.conversation_id)
        .bind(command.organization_id.as_str())
        .bind(command.actor.actor_type.as_str())
        .bind(command.actor.id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        if participant_exists.is_none() {
            transaction.rollback().await?;
            return Err(AppError::NotFound);
        }

        let current_version = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT version
            FROM dm.drafts
            WHERE conversation_id = $1
              AND organization_id = $2
              AND actor_kind = $3::text::dm.actor_kind
              AND actor_id = $4
            FOR UPDATE
            "#,
        )
        .bind(command.conversation_id)
        .bind(command.organization_id.as_str())
        .bind(command.actor.actor_type.as_str())
        .bind(command.actor.id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;

        match current_version {
            None if command.expected_version.unwrap_or(0) == 0 => {
                sqlx::query(
                    r#"
                    INSERT INTO dm.drafts (
                        conversation_id,
                        organization_id,
                        actor_kind,
                        actor_id,
                        text_content,
                        content_hash
                    )
                    VALUES ($1, $2, $3::text::dm.actor_kind, $4, $5, $6)
                    "#,
                )
                .bind(command.conversation_id)
                .bind(command.organization_id.as_str())
                .bind(command.actor.actor_type.as_str())
                .bind(command.actor.id.as_str())
                .bind(command.input.message_content.as_deref())
                .bind(content_hash.as_bytes().as_slice())
                .execute(&mut *transaction)
                .await
                .map_err(map_constraint_error)?;
            }
            None => {
                transaction.rollback().await?;
                return Err(AppError::conflict(
                    "draft was deleted after the supplied version was observed",
                ));
            }
            Some(current) if command.expected_version == Some(current) => {
                sqlx::query(
                    r#"
                    UPDATE dm.drafts
                    SET version = version + 1,
                        text_content = $5,
                        content_hash = $6
                    WHERE conversation_id = $1
                      AND organization_id = $2
                      AND actor_kind = $3::text::dm.actor_kind
                      AND actor_id = $4
                    "#,
                )
                .bind(command.conversation_id)
                .bind(command.organization_id.as_str())
                .bind(command.actor.actor_type.as_str())
                .bind(command.actor.id.as_str())
                .bind(command.input.message_content.as_deref())
                .bind(content_hash.as_bytes().as_slice())
                .execute(&mut *transaction)
                .await
                .map_err(map_constraint_error)?;

                delete_draft_children(&mut transaction, &command).await?;
            }
            Some(_) => {
                transaction.rollback().await?;
                let current = self
                    .load_draft(
                        &command.organization_id,
                        &command.actor,
                        command.conversation_id,
                    )
                    .await?;
                return Ok(PutDraftOutcome::Conflict(current));
            }
        }

        insert_draft_children(&mut transaction, &command).await?;
        let saved = load_draft_in(
            &mut transaction,
            &command.organization_id,
            &command.actor,
            command.conversation_id,
        )
        .await?;
        transaction.commit().await.map_err(map_constraint_error)?;
        Ok(PutDraftOutcome::Saved(saved))
    }

    /// Deletes the authenticated participant's draft, if present.
    ///
    /// # Errors
    ///
    /// Returns not-found for a non-participant and propagates database errors.
    pub async fn delete_draft(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<()> {
        self.require_participant(organization_id, actor, conversation_id)
            .await?;
        let mut transaction = self.pool().begin().await?;
        let advanced = sqlx::query(
            r#"
            UPDATE dm.drafts
            SET version = version + 1
            WHERE conversation_id = $1
              AND organization_id = $2
              AND actor_kind = $3::text::dm.actor_kind
              AND actor_id = $4
            "#,
        )
        .bind(conversation_id)
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        if advanced.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(());
        }

        delete_draft_children_by_owner(&mut transaction, organization_id, actor, conversation_id)
            .await?;
        sqlx::query(
            r#"
            DELETE FROM dm.drafts
            WHERE conversation_id = $1
              AND organization_id = $2
              AND actor_kind = $3::text::dm.actor_kind
              AND actor_id = $4
            "#,
        )
        .bind(conversation_id)
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        transaction.commit().await.map_err(map_constraint_error)
    }

    async fn load_draft(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<Draft> {
        let mut transaction = self.pool().begin().await?;
        let draft =
            load_draft_in(&mut transaction, organization_id, actor, conversation_id).await?;
        transaction.commit().await.map_err(map_constraint_error)?;
        Ok(draft)
    }
}

async fn load_draft_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actor: &ActorRef,
    conversation_id: Uuid,
) -> AppResult<Draft> {
    // Child mutations take a FOR UPDATE lock on this parent. Holding a shared
    // lock therefore keeps the root and its child rows on one coherent version
    // for the three hydration queries below.
    let record = sqlx::query_as::<_, DraftRecord>(
        r#"
        SELECT conversation_id, actor_id, version, text_content, updated_at
        FROM dm.drafts
        WHERE conversation_id = $1
          AND organization_id = $2
          AND actor_kind = $3::text::dm.actor_kind
          AND actor_id = $4
        FOR SHARE
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AppError::NotFound)?;

    let attachment_records = sqlx::query_as::<_, DraftAttachmentRecord>(
        r#"
        SELECT
            attachment_kind::text AS attachment_kind,
            permanent_url,
            name,
            content_type,
            declared_size_bytes
        FROM dm.draft_attachments
        WHERE conversation_id = $1
          AND organization_id = $2
          AND actor_kind = $3::text::dm.actor_kind
          AND actor_id = $4
        ORDER BY position
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .fetch_all(&mut **transaction)
    .await?;
    let gif_record = sqlx::query_as::<_, DraftGifRecord>(
        r#"
        SELECT provider_id, url, preview_url, title
        FROM dm.draft_gifs
        WHERE conversation_id = $1
          AND organization_id = $2
          AND actor_kind = $3::text::dm.actor_kind
          AND actor_id = $4
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .fetch_optional(&mut **transaction)
    .await?;

    let mut attachments = Vec::new();
    let mut voice = None;
    for item in attachment_records {
        let attachment = attachment_from_record(&item)?;
        match item.attachment_kind.as_str() {
            "voice" => voice = Some(attachment),
            "attachment" => attachments.push(attachment),
            _ => {
                return Err(AppError::internal(anyhow::anyhow!(
                    "invalid draft attachment kind loaded from DM database"
                )));
            }
        }
    }
    let gif = gif_record.map(gif_from_record).transpose()?;
    let actor_id = record.actor_id.parse().map_err(|error| {
        AppError::internal(anyhow::anyhow!(
            "invalid draft actor ID loaded from DM database: {error}"
        ))
    })?;
    Ok(Draft {
        conversation_id: record.conversation_id,
        actor_id,
        version: record.version,
        message_content: record.text_content,
        attachments,
        voice,
        gif,
        updated_at: record.updated_at,
    })
}

async fn delete_draft_children(
    transaction: &mut Transaction<'_, Postgres>,
    command: &PutDraftCommand,
) -> AppResult<()> {
    delete_draft_children_by_owner(
        transaction,
        &command.organization_id,
        &command.actor,
        command.conversation_id,
    )
    .await
}

async fn delete_draft_children_by_owner(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actor: &ActorRef,
    conversation_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"
        DELETE FROM dm.draft_attachments
        WHERE conversation_id = $1
          AND organization_id = $2
          AND actor_kind = $3::text::dm.actor_kind
          AND actor_id = $4
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    sqlx::query(
        r#"
        DELETE FROM dm.draft_gifs
        WHERE conversation_id = $1
          AND organization_id = $2
          AND actor_kind = $3::text::dm.actor_kind
          AND actor_id = $4
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}

async fn insert_draft_children(
    transaction: &mut Transaction<'_, Postgres>,
    command: &PutDraftCommand,
) -> AppResult<()> {
    for (position, attachment) in command.input.attachments.iter().enumerate() {
        insert_attachment(transaction, command, position, "attachment", attachment).await?;
    }
    if let Some(voice) = &command.input.voice {
        insert_attachment(transaction, command, 100, "voice", voice).await?;
    }
    if let Some(gif) = &command.input.gif {
        sqlx::query(
            r#"
            INSERT INTO dm.draft_gifs (
                conversation_id,
                organization_id,
                actor_kind,
                actor_id,
                provider_id,
                url,
                preview_url,
                title
            )
            VALUES ($1, $2, $3::text::dm.actor_kind, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(command.conversation_id)
        .bind(command.organization_id.as_str())
        .bind(command.actor.actor_type.as_str())
        .bind(command.actor.id.as_str())
        .bind(&gif.provider_id)
        .bind(gif.url.as_str())
        .bind(gif.preview_url.as_ref().map(url::Url::as_str))
        .bind(gif.title.as_deref())
        .execute(&mut **transaction)
        .await
        .map_err(map_constraint_error)?;
    }
    Ok(())
}

async fn insert_attachment(
    transaction: &mut Transaction<'_, Postgres>,
    command: &PutDraftCommand,
    position: usize,
    attachment_kind: &str,
    attachment: &Attachment,
) -> AppResult<()> {
    let position =
        i16::try_from(position).map_err(|error| AppError::internal(anyhow::Error::new(error)))?;
    let size = attachment
        .size
        .map(i64::try_from)
        .transpose()
        .map_err(|error| AppError::validation(error.to_string()))?;
    sqlx::query(
        r#"
        INSERT INTO dm.draft_attachments (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id,
            position,
            attachment_kind,
            permanent_url,
            name,
            content_type,
            declared_size_bytes
        )
        VALUES ($1, $2, $3::text::dm.actor_kind, $4, $5, $6::text::dm.attachment_kind, $7, $8, $9, $10)
        "#,
    )
    .bind(command.conversation_id)
    .bind(command.organization_id.as_str())
    .bind(command.actor.actor_type.as_str())
    .bind(command.actor.id.as_str())
    .bind(position)
    .bind(attachment_kind)
    .bind(attachment.permanent_url.as_str())
    .bind(attachment.name.as_deref())
    .bind(attachment.content_type.as_deref())
    .bind(size)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}

fn attachment_from_record(record: &DraftAttachmentRecord) -> AppResult<Attachment> {
    let size = record
        .declared_size_bytes
        .map(u64::try_from)
        .transpose()
        .map_err(|error| AppError::internal(anyhow::Error::new(error)))?;
    Ok(Attachment {
        permanent_url: parse_url(&record.permanent_url, "draft attachment permanent URL")?,
        name: record.name.clone(),
        content_type: record.content_type.clone(),
        size,
    })
}

fn gif_from_record(record: DraftGifRecord) -> AppResult<Gif> {
    Ok(Gif {
        provider_id: record.provider_id,
        url: parse_url(&record.url, "draft GIF URL")?,
        preview_url: record
            .preview_url
            .as_deref()
            .map(|value| parse_url(value, "draft GIF preview URL"))
            .transpose()?,
        title: record.title,
    })
}
