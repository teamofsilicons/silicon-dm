import type { TingConnection } from "./ting-session";

let current: TingConnection | null = null;
const listeners = new Set<() => void>();
export const tingConnection = () => current;
export const serverTingConnection = () => null;
export function subscribeTingConnection(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; }
export function setTingConnection(value: TingConnection | null) { current = value; for (const listener of listeners) listener(); }
