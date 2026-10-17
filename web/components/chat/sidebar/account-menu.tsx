"use client";

/**
 * The signed-in account behind its photo (Silicon UI's user menu): its name, its c: or si: id and whether it is a
 * Carbon or a Silicon, then Settings, Keyboard shortcuts, the account's page at Silicon Accounts, the docs, and sign
 * out. On phones it opens as a bottom sheet.
 */
import { useRouter } from "next/navigation";
import { BookOpen, CircleUserRound, Keyboard, Settings2 } from "lucide-react";
import { UserMenu } from "@/components/silicon-ui/user-menu/user-menu";
import { appConfig } from "@/lib/app.config";
import { useSignOut } from "@/lib/client/session";
import { kindNoun } from "@/lib/format";
import { notifyError } from "@/lib/notify";
import { chordCaps, chordsOf } from "@/lib/shortcuts/keys";
import { useShortcuts } from "@/lib/shortcuts/react";
import { openDialog } from "../dialogs/store";
import { useChat } from "../shell/chat-context";

export function AccountMenu() {
  const { account, accountsUrl } = useChat();
  const { signOut } = useSignOut();
  const router = useRouter();
  const { shortcuts, platform } = useShortcuts();
  const keysOf = (id: string) => {
    const binding = shortcuts.find(entry => entry.id === id)?.bindings[0];
    if (!binding) return undefined;
    return chordsOf(binding, platform).map(chord => chordCaps(chord, platform).map(cap => cap.label).join(platform === "mac" ? "" : "+"));
  };
  return (
    <UserMenu
      user={{ name: account.display_name || account.id, email: account.id, plan: kindNoun(account.kind), avatarSrc: account.pfp_url ?? undefined }}
      showTheme={false}
      showName
      align="start"
      onSignOut={async () => {
        try {
          await signOut();
        } catch (failure) {
          notifyError(failure, "Could not sign out");
        }
      }}
      items={[
        { label: "Settings", icon: <Settings2 size={16} strokeWidth={1.75} />, keys: keysOf("go.settings"), onSelect: () => router.push("/settings") },
        { label: "Keyboard shortcuts", icon: <Keyboard size={16} strokeWidth={1.75} />, keys: keysOf("shortcuts.open"), onSelect: () => openDialog("shortcuts") },
        { label: "Your account", icon: <CircleUserRound size={16} strokeWidth={1.75} />, onSelect: () => void window.open(accountsUrl, "_blank", "noopener") },
        { label: `${appConfig.name} docs`, icon: <BookOpen size={16} strokeWidth={1.75} />, onSelect: () => void window.open(appConfig.links.docs, "_blank", "noopener") },
      ]}
    />
  );
}
