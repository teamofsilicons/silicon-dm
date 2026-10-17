"use client";

import { useSyncExternalStore } from "react";

function subscribe(listener: () => void) {
  window.addEventListener("online", listener);
  window.addEventListener("offline", listener);
  return () => {
    window.removeEventListener("online", listener);
    window.removeEventListener("offline", listener);
  };
}

/** Whether the browser says it is online (true on the server and before hydration, so nothing flashes "offline"). */
export function useOnline(): boolean {
  return useSyncExternalStore(subscribe, () => navigator.onLine !== false, () => true);
}
