/**
 * Voice failures in a Carbon's words (spec §2 rule 5: what happened, why, what to do). The service writes its errors
 * for Silicons and for whoever runs it: they name routes and settings ("choose another voice from GET /api/v1/voices",
 * "the operator sets DM_DEEPGRAM_API_KEY", "retry the same request after the Retry-After delay"). Listening, a voice
 * sample, a voice message's transcript and dictation show these words instead. The code stays the service's, so what
 * branches on it still works (a network failure still falls back to the copy this browser kept). Anything not listed
 * here keeps the service's own words, which already read well (a deleted message, nothing to read aloud).
 */
import { ApiError } from "@/lib/errors";

/**
 * What the Carbon was doing: hearing a message, hearing a voice's sample, a voice message's words, dictation, or
 * looking at and choosing voices (Settings → Voice, the details panel).
 */
export type VoiceTask = "listen" | "sample" | "transcribe" | "dictate" | "voices";

interface Words {
  message: string;
  hint: string;
}

/** How long the service asked to wait (Retry-After, or the limit's own `retry_after_seconds`), in whole seconds. */
function waitSeconds(error: ApiError): number | null {
  const value = error.retryAfter ?? Number(error.details.retry_after_seconds);
  return Number.isFinite(value) && value > 0 ? Math.ceil(value) : null;
}

/** The limit the service names (`details.limit`), written out ("30", "200,000"), or null. */
function limitOf(error: ApiError): string | null {
  const value = Number(error.details.limit);
  return Number.isFinite(value) && value > 0 ? value.toLocaleString("en-US") : null;
}

function tryAgain(error: ApiError, otherwise: string): string {
  const wait = waitSeconds(error);
  if (!wait) return otherwise;
  if (wait >= 90 * 60) return `Try again in about ${Math.round(wait / 3600)} hours.`;
  if (wait >= 90) return `Try again in about ${Math.round(wait / 60)} minutes.`;
  return `Try again in ${wait} ${wait === 1 ? "second" : "seconds"}.`;
}

function wordsFor(error: ApiError, task: VoiceTask): Words | null {
  const hearing = task === "listen" || task === "sample";
  const offline = typeof navigator !== "undefined" && navigator.onLine === false;
  if (task === "voices") {
    switch (error.code) {
      case "network_error":
        return offline ? { message: "This browser is offline.", hint: "Try again once you are back online." } : { message: "DM could not be reached.", hint: "Check the connection, then try again." };
      case "server_unavailable":
      case "unexpected_response":
        return { message: "DM did not answer as it should.", hint: "Try again in a moment." };
      case "voice_not_found":
        return { message: "That voice is not one DM offers any more.", hint: "Choose one of the voices shown." };
      default:
        return null;
    }
  }
  switch (error.code) {
    case "network_error":
      if (hearing) {
        return offline
          ? { message: "The audio could not be loaded: this browser is offline.", hint: "Try again once you are back online." }
          : { message: "The audio could not be loaded: DM could not be reached.", hint: "Check the connection, then try again." };
      }
      return offline
        ? { message: "The recording could not be turned into words: this browser is offline.", hint: "Try again once you are back online." }
        : { message: "The recording could not be turned into words: DM could not be reached.", hint: "Check the connection, then try again." };
    case "server_unavailable":
    case "unexpected_response":
      return hearing
        ? { message: "The audio could not be loaded: DM did not answer as it should.", hint: "Try again in a moment." }
        : { message: "The recording could not be turned into words: DM did not answer as it should.", hint: "Try again in a moment." };
    case "speech_rejected":
      return task === "sample"
        ? { message: "This voice could not say its sample.", hint: "Try again later, or listen to another voice." }
        : { message: "The voice could not say these words, so there is no audio for them.", hint: "Read the message instead. If it keeps happening, the Silicon's custodian can choose another voice in Settings → Voice." };
    case "speech_unavailable":
      return hearing
        ? { message: "The audio could not be prepared: the voice did not answer in time.", hint: `Nothing is lost. ${tryAgain(error, "Try again in a few seconds.")}` }
        : { message: "The recording could not be turned into words: the transcription did not answer in time.", hint: tryAgain(error, "Try again in a few seconds.") };
    case "speech_not_configured":
      if (hearing) return { message: "Listening is not set up on this DM service yet.", hint: task === "sample" ? "Voices can be chosen, but not heard here yet." : "Read the messages instead." };
      return { message: "Turning speech into words is not set up on this DM service yet.", hint: task === "dictate" ? "Type instead." : "Voice messages still go, without their words." };
    case "speech_rate_limited": {
      const limit = limitOf(error);
      return { message: `DM makes ${limit ? `at most ${limit}` : "only so many"} new recordings a minute for each account, and you reached that.`, hint: `${tryAgain(error, "Try again in a minute.")} Anything you already heard still plays.` };
    }
    case "speech_budget_exceeded": {
      const limit = limitOf(error);
      return { message: `DM reads ${limit ? `at most ${limit} characters` : "only so much"} aloud a day for each account, and you reached that.`, hint: "Read messages instead until midnight UTC, when it starts again. Anything you already heard still plays." };
    }
    case "transcription_rate_limited": {
      const limit = limitOf(error);
      return { message: `DM turns ${limit ? `at most ${limit}` : "only so many"} recordings into words a minute for each account, and you reached that.`, hint: tryAgain(error, "Try again in a minute.") };
    }
    case "audio_too_large":
      return { message: "The recording is too long to be turned into words.", hint: task === "dictate" ? "Dictate a shorter part, then the rest." : "Record a shorter one." };
    case "unsupported_audio_type":
      return { message: "This browser records audio in a form DM cannot turn into words.", hint: "Use a recent Chrome, Edge, Firefox or Safari." };
    default:
      return null;
  }
}

/** A voice message's words failed: it has gone without them (there is nothing to try again), and says so. */
const WENT_WITHOUT = "The voice message went without its words; it plays as recorded.";

/** The failure as a Carbon reads it (the same error, with the words above where the service's are not for Carbons). */
export function voiceWords(error: ApiError, task: VoiceTask): ApiError {
  const found = wordsFor(error, task);
  if (!found) return error;
  const words = task !== "transcribe" ? found : { message: found.message, hint: error.code === "unsupported_audio_type" ? `${found.hint} ${WENT_WITHOUT}` : WENT_WITHOUT };
  return new ApiError({
    status: error.status,
    code: error.code,
    message: words.message,
    hint: words.hint,
    details: error.details,
    requestId: error.requestId,
    retryAfter: error.retryAfter,
    method: error.method,
    path: error.path,
    cause: error,
  });
}
