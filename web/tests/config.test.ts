import assert from "node:assert/strict";
import test from "node:test";
import { configuration } from "../server/config.ts";

test("DM gateway accepts bare application IDs and rejects legacy org-qualified IDs", () => {
  assert.equal(configuration({}).appId, "dm");
  assert.equal(configuration({ DM_WEB_APP_ID: "dm-preview" }).appId, "dm-preview");
  for (const id of ["tos>dm", "9dm", "DM", "dm.name", "a".repeat(81)]) {
    assert.throws(() => configuration({ DM_WEB_APP_ID: id }), /Invalid canonical DM app ID/);
  }
});
