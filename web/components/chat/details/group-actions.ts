"use client";

/**
 * What an owner (or a member) does to a group from the details panel, each as one call to the service whose answer
 * goes straight into the caches the panel, the thread header and the conversation list read: the group's details, the
 * conversation, and its row in the list. Errors are thrown as the service's ApiError for the caller to show.
 *
 * A rename or a new description sends the group's version (If-Match). When someone else changed the group in the
 * meantime the service answers 409; the change is then made again on the current version, keeping the other field
 * as it is now (theirs), so nobody's edit is lost.
 */
import { useCallback, useMemo } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { addOwners, getGroup, inviteMembers, leaveGroup, removeMembers, removeOwners, updateGroup } from "@/lib/chat/api";
import type { ConversationPages } from "@/lib/chat/cache";
import { chatKeys } from "@/lib/chat/keys";
import type { ConversationSummary, GroupDetails } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";

type GroupData = ConversationSummary & { group: GroupDetails };

/** Writes a group's details into the conversation and its row in the list (the thread header and the sidebar read them). */
export function applyGroupToSummaries(queryClient: QueryClient, id: string, view: string | null, details: GroupDetails): void {
  const merge = (summary: ConversationSummary): ConversationSummary =>
    summary.id === id || summary.supersedes?.includes(id) ? { ...summary, group: { ...summary.group, ...details }, participants: details.members.length ? details.members : summary.participants } : summary;
  queryClient.setQueryData<ConversationSummary>(chatKeys.conversation(id, view), data => (data ? merge(data) : data));
  queryClient.setQueryData<ConversationPages>(chatKeys.conversations(view), data => (data ? { ...data, pages: data.pages.map(page => ({ ...page, items: page.items.map(merge) })) } : data));
}

/** Writes a group's details wherever they are cached. */
export function applyGroup(queryClient: QueryClient, id: string, view: string | null, details: GroupDetails): void {
  queryClient.setQueryData<GroupData>(chatKeys.group(id, view), data => (data ? { ...data, group: details, participants: details.members.length ? details.members : data.participants } : data));
  applyGroupToSummaries(queryClient, id, view, details);
}

/** True when the conversation's copy of the group says something else than the group's own details (a rename elsewhere). */
export function summaryDiffers(summary: ConversationSummary["group"] | undefined, details: GroupDetails): boolean {
  if (!summary) return false;
  const owners = (list: Array<{ id: string; uuid?: string }>) => list.map(owner => owner.uuid ?? owner.id).sort().join(" ");
  return summary.name !== details.name || summary.description !== details.description || owners(summary.owners) !== owners(details.owners);
}

/** Takes a group out of the list (after leaving it). */
function dropGroup(queryClient: QueryClient, id: string, view: string | null): void {
  queryClient.setQueryData<ConversationPages>(chatKeys.conversations(view), data => (data ? { ...data, pages: data.pages.map(page => ({ ...page, items: page.items.filter(item => item.id !== id) })) } : data));
  queryClient.removeQueries({ queryKey: chatKeys.group(id, view) });
  queryClient.removeQueries({ queryKey: chatKeys.conversation(id, view) });
}

export interface GroupActions {
  /** Renames and/or describes the group (owners). */
  update: (change: { name?: string; description?: string }) => Promise<GroupDetails>;
  invite: (ids: string[]) => Promise<GroupDetails>;
  /** Removes members, or withdraws invitations. */
  remove: (ids: string[]) => Promise<GroupDetails>;
  setOwner: (id: string, owner: boolean) => Promise<GroupDetails>;
  leave: () => Promise<void>;
}

export function useGroupActions(id: string, view: string | null, current: GroupDetails | undefined): GroupActions {
  const queryClient = useQueryClient();
  const apply = useCallback((details: GroupDetails) => {
    applyGroup(queryClient, id, view, details);
    return details;
  }, [queryClient, id, view]);

  const update = useCallback(async (change: { name?: string; description?: string }) => {
    const base = current ?? (await getGroup(id, { view })).group;
    const body = (from: GroupDetails) => ({ name: change.name ?? from.name, description: change.description ?? from.description });
    try {
      return apply(await updateGroup(id, body(base), base.version));
    } catch (failure) {
      if (!ApiError.is(failure) || failure.status !== 409) throw failure;
      const fresh = (await getGroup(id, { view })).group;
      return apply(await updateGroup(id, body(fresh), fresh.version));
    }
  }, [apply, current, id, view]);

  return useMemo<GroupActions>(() => ({
    update,
    invite: async ids => apply(await inviteMembers(id, ids)),
    remove: async ids => apply(await removeMembers(id, ids)),
    setOwner: async (member, owner) => apply(await (owner ? addOwners(id, [member]) : removeOwners(id, [member]))),
    leave: async () => {
      await leaveGroup(id);
      dropGroup(queryClient, id, view);
      void queryClient.invalidateQueries({ queryKey: chatKeys.conversations(view) });
    },
  }), [apply, id, queryClient, update, view]);
}
