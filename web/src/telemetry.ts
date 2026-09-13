import type { Session } from './models';
import {getGeneration} from './storage';
const preference = 'silicon-dm.telemetry';
export function telemetryEnabled(): boolean {
  try { return typeof localStorage === 'undefined' || localStorage.getItem(preference) !== 'off'; }
  catch { return false; }
}
export function setTelemetryEnabled(enabled: boolean): void {
  localStorage.setItem(preference, enabled ? 'on' : 'off');
  window.dispatchEvent(new Event('dm-telemetry-change'));
}
/** Explicit analytics/diagnostics: no text, URLs, DOM, cookies, tokens or form fields. */
export function recordTelemetry(session: Session, origin: string, event: 'page_view'|'request'|'web_error', success = true, duration = 0): void {
  if (!telemetryEnabled() || !session.authenticated || !session.profile_id) return;
  const profileId = session.profile_id;
  void (async () => {
    const generation = session.testing_environment_id ? await getGeneration(session) : undefined;
    if (session.testing_environment_id && generation == null) return;
    if (!telemetryEnabled()) return;
    await fetch(new URL('/api/dm/telemetry', origin), {
    method:'POST', credentials:'include', redirect:'error', cache:'no-store',
    signal:AbortSignal.timeout(1000),
    headers:{'Content-Type':'application/json','X-DM-Profile':profileId,'X-DM-Source':'web',...(generation == null ? {} : {'X-Testing-Environment-Generation':String(generation)})},
    body:JSON.stringify({type:'telemetry',data:{source:'web',event,success,duration_ms:Math.min(604800000,Math.max(0,Math.round(duration)))}}),
    });
  })().catch(()=>{});
}
