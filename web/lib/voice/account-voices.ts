"use client";

/**
 * Accounts' voices (spec §4.2, §6.6): reading one (`GET /api/v1/voices/{account}`), choosing one (`PUT`, saved a moment
 * after the last change, so arrowing through the five voices sends one request), and going back to the default
 * (`DELETE`). The choice shows at once; when the service refuses (403 voice_change_forbidden, say), the voice it had
 * comes back and the service's words say why.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getVoice, resetVoice, setVoice } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import type { AccountRef, AccountVoice, VoiceOption } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { forgetSpeaker } from "./sources";
import { defaultVoice } from "./voices";

const SAVE_AFTER_MS = 450;

export function useAccountVoice(account: string | null) {
  return useQuery({
    queryKey: chatKeys.voice(account ?? ""),
    queryFn: ({ signal }) => getVoice(account!, signal),
    enabled: !!account,
    staleTime: 60_000,
    retry: (count, error) => ApiError.from(error).status >= 500 && count < 2,
    meta: { toast: false },
  });
}

/** The voice an account has when nothing is chosen (computed with the service's rule), or null while unknown. */
export function useDefaultVoiceKey(account: Pick<AccountRef, "uuid"> | null | undefined, voices: readonly VoiceOption[] | undefined): string | null {
  const uuid = account?.uuid ?? "";
  const keys = (voices ?? []).map(voice => voice.key).join(",");
  const query = useQuery({
    queryKey: ["dm", "voice-default", uuid, keys],
    queryFn: async () => (await defaultVoice({ uuid }, voices ?? []))?.key ?? null,
    enabled: !!uuid && !!voices?.length,
    staleTime: Infinity,
    gcTime: Infinity,
    meta: { toast: false },
  });
  return query.data ?? null;
}

export interface VoiceChoice {
  /** Shows `voice` at once and saves it a moment later. */
  choose: (voice: VoiceOption) => void;
  /** Back to the default voice. */
  reset: () => Promise<void>;
  /** Why the last change was refused (the service's words), or null. */
  error: ApiError | null;
  saving: boolean;
}

export function useVoiceChoice(account: string, me: AccountRef): VoiceChoice {
  const queryClient = useQueryClient();
  const [error, setError] = useState<ApiError | null>(null);
  const [saving, setSaving] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pending = useRef<(() => void) | null>(null);
  const confirmed = useRef<AccountVoice | undefined>(undefined);
  const generation = useRef(0);

  const save = useCallback(async (voice: VoiceOption, mine: number) => {
    const key = chatKeys.voice(account);
    setSaving(true);
    try {
      const saved = await setVoice(account, voice.key);
      if (generation.current === mine) queryClient.setQueryData(key, saved);
      forgetSpeaker(saved.account);
    } catch (failure) {
      if (generation.current !== mine) return;
      queryClient.setQueryData(key, confirmed.current);
      setError(ApiError.from(failure));
    } finally {
      if (generation.current === mine) setSaving(false);
    }
  }, [account, queryClient]);

  const choose = useCallback((voice: VoiceOption) => {
    const key = chatKeys.voice(account);
    const current = queryClient.getQueryData<AccountVoice>(key);
    if (!current) return;
    if (!timer.current) confirmed.current = current;
    queryClient.setQueryData<AccountVoice>(key, { ...current, voice, chosen: true, chosen_by: me, updated_at: new Date().toISOString() });
    setError(null);
    if (timer.current) clearTimeout(timer.current);
    const mine = ++generation.current;
    const run = () => {
      timer.current = null;
      pending.current = null;
      void save(voice, mine);
    };
    pending.current = run;
    timer.current = setTimeout(run, SAVE_AFTER_MS);
  }, [account, me, queryClient, save]);

  const reset = useCallback(async () => {
    const key = chatKeys.voice(account);
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    pending.current = null;
    const mine = ++generation.current;
    setError(null);
    setSaving(true);
    try {
      const saved = await resetVoice(account);
      if (generation.current === mine) queryClient.setQueryData(key, saved);
      forgetSpeaker(saved.account);
    } catch (failure) {
      if (generation.current === mine) setError(ApiError.from(failure));
    } finally {
      if (generation.current === mine) setSaving(false);
    }
  }, [account, queryClient]);

  // A choice still waiting when the picker closes is saved now, not lost.
  const flush = useCallback(() => {
    const run = pending.current;
    if (!run) return;
    if (timer.current) clearTimeout(timer.current);
    run();
  }, []);
  useEffect(() => flush, [flush]);

  return { choose, reset, error, saving };
}
