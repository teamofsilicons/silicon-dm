/**
 * The words a failed request shows (lib/errors.ts): what happened and what to do, for the Carbon reading them. The
 * request itself (method and path) never reaches the words; it stays on the error's fields for logs and code.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError, errorFromResponse, networkError, unreachableWords } from "../lib/errors";

const failedFetch = new TypeError("Failed to fetch");

test("a request that never got an answer says so in a Carbon's words, and keeps the request in its fields", () => {
  const read = networkError(failedFetch, "GET", "/api/v1/conversations/g%3Ax/messages", "this site");
  assert.equal(read.code, "network_error");
  assert.equal(read.status, 0);
  assert.equal(read.message, "DM can't reach its service right now.");
  assert.equal(read.hint, "It keeps trying; check your connection if this lasts.");
  assert.equal(read.method, "GET");
  assert.equal(read.path, "/api/v1/conversations/g%3Ax/messages");
  assert.equal(read.cause, failedFetch, "the browser's own reason stays on the error for logs");
  assert.doesNotMatch(`${read.message} ${read.hint}`, /GET|\/api|fetch|this site/i);

  // A change is not sent again by itself, so its words say to try again.
  const change = networkError(failedFetch, "POST", "/api/v1/groups/g%3Ax/accept", "this site");
  assert.equal(change.message, "DM can't reach its service right now.");
  assert.equal(change.hint, "Check your connection, then try again.");

  // A cancelled request is not a failure to reach anything.
  const aborted = networkError(Object.assign(new Error("The operation was aborted."), { name: "AbortError" }), "GET", "/api/v1/sync");
  assert.equal(aborted.code, "aborted");
});

test("an offline browser says it is offline", () => {
  assert.deepEqual(unreachableWords("GET", true), { message: "This browser is offline, so DM can't reach its service.", hint: "DM catches up once you are back online." });
  assert.deepEqual(unreachableWords("DELETE", true), { message: "This browser is offline, so DM can't reach its service.", hint: "Try again once you are back online." });
  assert.equal(unreachableWords("HEAD", false).hint, "It keeps trying; check your connection if this lasts.");
});

test("an answer without error details is put in words from its status, never the request", () => {
  const gateway = errorFromResponse(new Response("<html>Bad gateway</html>", { status: 502 }), null, "GET", "/api/v1/conversations");
  assert.equal(gateway.code, "server_unavailable");
  assert.equal(gateway.message, "DM's service is not answering right now (HTTP 502).");
  assert.equal(gateway.hint, "Try again in a moment.");
  assert.equal(gateway.path, "/api/v1/conversations");
  const large = errorFromResponse(new Response("", { status: 413 }), null, "POST", "/api/v1/conversations/x/messages");
  assert.equal(large.message, "That is too large for DM's service to take (HTTP 413).");
  assert.equal(errorFromResponse(new Response("", { status: 418 }), null, "GET", "/x").message, "DM's service answered in a way this page did not expect (HTTP 418).");
  assert.equal(errorFromResponse(new Response("", { status: 599 }), null, "GET", "/x").message, "DM's service ran into a problem (HTTP 599).");
  assert.equal(errorFromResponse(new Response("", { status: 401 }), null, "GET", "/x").code, "signed_out");
  for (const error of [gateway, large]) assert.doesNotMatch(`${error.message} ${error.hint}`, /GET|POST|\/api/);
});

test("the service's own words are kept as they are", () => {
  const body = { type: "error", data: { error: { code: "first_contact_limit", message: "You started 100 new conversations in the last hour, the most DM allows.", hint: "The next new conversation fits in 12 minutes (Retry-After)." } } };
  const refused = errorFromResponse(new Response(JSON.stringify(body), { status: 429 }), body, "POST", "/api/v1/conversations");
  assert.equal(refused.code, "first_contact_limit");
  assert.equal(refused.message, "You started 100 new conversations in the last hour, the most DM allows.");
  assert.equal(refused.hint, "The next new conversation fits in 12 minutes (Retry-After).");
  assert.ok(ApiError.is(refused, "first_contact_limit"));
});
