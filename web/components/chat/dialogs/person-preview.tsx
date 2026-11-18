"use client";
import { AccountChip, type AccountChipProps } from "@/components/foundation/account/account-chip";
import { useExactPerson } from "@/lib/chat/contacts";
import type { AccountRef } from "@/lib/chat/types";
export function PersonPreview({account,...props}:{account:AccountRef} & Omit<AccountChipProps,"account">) {
  const person=useExactPerson(account.id,account) ?? account;
  return <AccountChip account={{...person,kind:person.type}} {...props}/>;
}
