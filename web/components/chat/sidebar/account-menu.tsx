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
import { formatBinding } from "@/lib/shortcuts/keys";
import { useShortcuts } from "@/lib/shortcuts/react";
import { askToSignOut } from "../dialogs/sign-out-dialog";
import { openDialog } from "../dialogs/store";
import { useChat } from "../shell/chat-context";
import styles from "./sidebar.module.css";

export function AccountMenu() {
  const { account, accountsUrl } = useChat();
  const { signOut } = useSignOut();
  const router = useRouter();
  const { shortcuts, platform } = useShortcuts();
  // The keys as the cheatsheet and the palette write them ("G then S", "⌘K"), as one cap: the menu draws each cap it
  // is given side by side, and a sequence split into caps read "G S".
  const keysOf = (id: string) => {
    const binding = shortcuts.find(entry => entry.id === id)?.bindings[0];
    return binding ? [formatBinding(binding, platform).text] : undefined;
  };
  return (
    <UserMenu
      user={{ name: account.display_name || account.id, email: account.id, plan: kindNoun(account.kind), avatarSrc: account.pfp_url ?? undefined }}
      showTheme={false}
      showName
      className={styles.account}
      align="start"
      // Messages not sent yet are said first (dialogs/sign-out-dialog.tsx): signing out removes them from this browser.
      onSignOut={() => askToSignOut(signOut)}
      items={[
        { label: "Settings", icon: <Settings2 size={16} strokeWidth={1.75} />, keys: keysOf("go.settings"), onSelect: () => router.push("/settings") },
        { label: "Keyboard shortcuts", icon: <Keyboard size={16} strokeWidth={1.75} />, keys: keysOf("shortcuts.open"), onSelect: () => openDialog("shortcuts") },
        { label: "Your account", icon: <CircleUserRound size={16} strokeWidth={1.75} />, onSelect: () => void window.open(accountsUrl, "_blank", "noopener") },
        { label: `${appConfig.name} docs`, icon: <BookOpen size={16} strokeWidth={1.75} />, onSelect: () => void window.open(appConfig.links.docs, "_blank", "noopener") },
      ]}
    />
  );
}
