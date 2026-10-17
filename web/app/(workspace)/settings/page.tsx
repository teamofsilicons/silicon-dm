/** /settings: Voice, Listening, Sending, Keyboard, Notifications, your Silicons' reach, Account, Report a problem. */
import type { Metadata } from "next";
import { SettingsPage } from "@/components/chat/settings/settings-page";

export const metadata: Metadata = { title: "Settings" };

export default function Settings() {
  return <SettingsPage />;
}
