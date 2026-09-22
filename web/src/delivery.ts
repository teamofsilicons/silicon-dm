import { api } from "./api";
import type { Session } from "./models";
import {
  completeDeliveryRegistration,
  deliveryRegistrationKey,
  getGeneration,
  StorageError,
} from "./storage";
/** Called only by explicit UI actions. Ordinary retry retains the persisted key;
 * beginning another registration is a separate action because it may revive a grant. */
export async function registerDelivery(
  session: Session,
  startNew = false,
): Promise<void> {
  const generation = await getGeneration(session);
  if (generation === undefined)
    throw new StorageError(
      "generation_unknown",
      "Load this testing environment before enabling delivery.",
    );
  let key = await deliveryRegistrationKey(session, generation);
  if (startNew) {
    await completeDeliveryRegistration(session, generation, key);
    key = await deliveryRegistrationKey(session, generation);
  }
  await api("/delivery/registration", {
    method: "POST",
    body: {},
    session,
    generation,
    idempotencyKey: key,
  });
  await completeDeliveryRegistration(session, generation, key);
}
