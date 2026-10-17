"use client";

/** Whether this website can upload (GET /api/uploads/config), asked once per page and shared by every composer. */
import { useQuery } from "@tanstack/react-query";
import { getUploadsConfig, type UploadsConfig } from "@/lib/upload/client";

export const uploadsConfigKey = ["dm", "uploads", "config"] as const;

/**
 * Undefined until the answer is in (the composer then assumes uploads work; the upload itself would say otherwise). The
 * website's answer holds for the page; an unknown one (the page loaded offline: no size known) is asked again when the
 * connection is back, or when a composer next opens.
 */
export function useUploadsConfig(): UploadsConfig | undefined {
  return useQuery({
    queryKey: uploadsConfigKey,
    queryFn: ({ signal }) => getUploadsConfig(signal),
    staleTime: query => (query.state.data && query.state.data.max_bytes > 0 ? Infinity : 0),
    gcTime: Infinity,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: true,
    meta: { toast: false },
  }).data;
}

/** Why uploads are off here, or null when they work (or are not known yet). */
export function uploadsOffReason(config: UploadsConfig | undefined): string | null {
  if (!config || config.enabled) return null;
  return config.reason ?? "Uploads are off on this DM website.";
}
