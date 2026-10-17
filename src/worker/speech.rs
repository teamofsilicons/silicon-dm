//! Durable pre-rendering of spoken messages, and keeping the speech cache within its limits.
//!
//! A spoken message queues its rendering in the transaction that accepts it. This worker
//! renders queued messages through the speech provider (a few at a time), stores the audio in
//! the cache and announces the message to its conversation, so clients refresh it and find
//! its duration and waveform. Failures retry with backoff; after five attempts the message is
//! marked `failed` (listening still tries again on demand). Without a speech provider the
//! queue waits untouched.

use std::{future::pending, time::Duration};

use sqlx::postgres::PgListener;
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at};
use tokio_util::sync::CancellationToken;

use crate::{
    AppError, AppResult,
    application::{speech::render, state::AppState},
    infrastructure::postgres::{MAX_RENDER_ATTEMPTS, RenderClaim, RenderOutcome},
};

/// Renders running at once in one process.
const CONCURRENT_RENDERS: i64 = 4;
/// Longer than one rendering may take (three minutes).
const RENDER_LEASE: Duration = Duration::from_secs(240);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);
/// Waits before the second to fifth attempt.
const RETRY_DELAYS: [i64; 4] = [5, 30, 120, 600];

/// Renders queued spoken messages and evicts old audio.
pub struct SpeechWorker {
    state: AppState,
}

impl SpeechWorker {
    /// A worker over the application's store and speech provider.
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Runs until shutdown.
    ///
    /// # Errors
    /// Durable-storage failures.
    pub async fn run(&self, cancellation: CancellationToken) -> AppResult<()> {
        let Some(mut listener) = cancellation.run_until_cancelled(self.open_listener()).await
        else {
            return Ok(());
        };
        let mut poll = interval(POLL_INTERVAL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut maintenance = interval_at(Instant::now(), MAINTENANCE_INTERVAL);
        maintenance.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            let maintain = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Ok(()),
                _ = maintenance.tick() => true,
                _ = poll.tick() => false,
                notification = receive(&mut listener) => {
                    if notification.is_err() {
                        tracing::warn!(failure = "speech_listener",
                            "PostgreSQL speech wakeup listener disconnected; polling remains active");
                        listener = None;
                    }
                    false
                }
            };
            let result = if maintain {
                let Some(result) = cancellation.run_until_cancelled(self.maintain_once()).await
                else {
                    return Ok(());
                };
                if listener.is_none() {
                    listener = self.open_listener().await;
                }
                result.map(|_| ())
            } else {
                let Some(result) = cancellation.run_until_cancelled(self.process_once()).await
                else {
                    return Ok(());
                };
                result.map(|_| ())
            };
            if let Err(error) = result {
                // A database hiccup must not stop speech for good; the next tick retries.
                tracing::warn!(code = error.code(), "speech worker pass failed; retrying");
            }
        }
    }

    async fn open_listener(&self) -> Option<PgListener> {
        match self.state.store.subscribe_speech_wakeups().await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(
                    code = error.code(),
                    "PostgreSQL speech wakeup listener is unavailable; polling remains active"
                );
                None
            }
        }
    }

    /// Renders the queued messages that are due (a few at a time). Returns how many were
    /// processed.
    ///
    /// # Errors
    /// Durable-storage failures.
    pub async fn process_once(&self) -> AppResult<usize> {
        if !self.state.speech.enabled() {
            return Ok(0);
        }
        let mut processed = 0;
        loop {
            let claims = self
                .state
                .store
                .claim_speech_renders(CONCURRENT_RENDERS, RENDER_LEASE)
                .await?;
            if claims.is_empty() {
                return Ok(processed);
            }
            processed += claims.len();
            let results =
                futures::future::join_all(claims.into_iter().map(|claim| self.render_one(claim)))
                    .await;
            for result in results {
                result?;
            }
        }
    }

    async fn render_one(&self, claim: RenderClaim) -> AppResult<()> {
        let store = &self.state.store;
        if store.message_deleted(claim.message_id).await? {
            return store
                .finish_speech_render(
                    &claim,
                    RenderOutcome::Failed {
                        error: "message_deleted",
                    },
                )
                .await;
        }
        let outcome = match render(
            &self.state,
            None,
            &claim.script,
            &claim.voice_id,
            &claim.model,
        )
        .await
        {
            Ok(rendered) => RenderOutcome::Ready(rendered.meta),
            Err(error) => {
                let code = error.code();
                tracing::warn!(message_id = %claim.message_id, code, attempts = claim.attempts + 1,
                    "rendering a spoken message failed");
                outcome_of_failure(&error, claim.attempts)
            }
        };
        store.finish_speech_render(&claim, outcome).await
    }

    /// Evicts audio beyond the cache's size or age and drops expired usage windows.
    ///
    /// # Errors
    /// Durable-storage failures.
    pub async fn maintain_once(&self) -> AppResult<u64> {
        let speech = &self.state.settings.speech;
        let evicted = self
            .state
            .store
            .evict_speech_audio(speech.cache_max_bytes, speech.cache_ttl)
            .await?;
        let pruned = self.state.store.prune_speech_usage().await?;
        if evicted > 0 || pruned > 0 {
            tracing::info!(
                evicted_renderings = evicted,
                pruned_usage_windows = pruned,
                "speech cache maintenance completed"
            );
        }
        Ok(evicted)
    }
}

/// A failure the provider will refuse again fails at once; anything else retries with
/// backoff until the fifth attempt.
fn outcome_of_failure(error: &AppError, attempts: i32) -> RenderOutcome {
    let code = error.code();
    if code == "speech_rejected" || attempts + 1 >= MAX_RENDER_ATTEMPTS {
        return RenderOutcome::Failed { error: code };
    }
    let delay = RETRY_DELAYS[usize::try_from(attempts)
        .unwrap_or(0)
        .min(RETRY_DELAYS.len() - 1)];
    RenderOutcome::Retry {
        error: code,
        next_attempt_at: OffsetDateTime::now_utc() + time::Duration::seconds(delay),
    }
}

async fn receive(listener: &mut Option<PgListener>) -> Result<(), sqlx::Error> {
    match listener {
        Some(listener) => listener.recv().await.map(|_| ()),
        None => pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::outcome_of_failure;
    use crate::{AppError, infrastructure::postgres::RenderOutcome};

    #[test]
    fn refusals_fail_at_once_and_other_failures_retry_five_times() {
        let refused = AppError::refused(
            http::StatusCode::UNPROCESSABLE_ENTITY,
            "speech_rejected",
            "no",
        );
        assert!(matches!(
            outcome_of_failure(&refused, 0),
            RenderOutcome::Failed {
                error: "speech_rejected"
            }
        ));
        let unavailable = AppError::refused(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "speech_unavailable",
            "later",
        );
        for attempts in 0..4 {
            assert!(
                matches!(
                    outcome_of_failure(&unavailable, attempts),
                    RenderOutcome::Retry { .. }
                ),
                "attempt {attempts}"
            );
        }
        assert!(matches!(
            outcome_of_failure(&unavailable, 4),
            RenderOutcome::Failed {
                error: "speech_unavailable"
            }
        ));
    }
}
