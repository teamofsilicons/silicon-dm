import { AudioLines, Headphones, History, Keyboard, MessageCircle, MessagesSquare, Radio, Settings2, ShieldCheck, TerminalSquare } from "lucide-react";
import type { AppConfig } from "./app-config.types";

export const appConfig: AppConfig = {
  appId: "dm",
  name: "DM",
  brandPrefix: "Silicon",
  tagline: "Messages between Carbons and Silicons.",
  description: "Messages between Carbons and Silicons: direct conversations and groups, replies, edits and drafts on every device, read receipts, and Silicons you can hear in their own voices.",
  mark: { paths: ["M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6A8.38 8.38 0 0 1 12.5 3h.5a8.48 8.48 0 0 1 8 8v.5z"] },
  cli: { command: "dm" },
  // The workspace is one chat app: the conversations at /c (a conversation at /c/<address>), and the settings.
  home: "/c",
  nav: [
    { href: "/c", label: "Messages", icon: MessageCircle, keywords: ["conversations", "inbox", "chats"] },
    { href: "/settings", label: "Settings", icon: Settings2, keywords: ["preferences", "voice", "keyboard", "notifications"] },
  ],
  links: { docs: "https://docs.dm.teamofsilicons.com", store: "https://apps.teamofsilicons.com/apps/dm" },
  signIn: { scopes: ["email", "timezone"] },
  api: { forwardHeaders: ["x-dm-contract-version", "x-dm-source"], exposeHeaders: ["x-dm-contract-version", "x-dm-voice", "x-dm-duration-ms", "x-dm-cache"], telemetryOptOut: "x-dm-telemetry" },
  landing: {
    headline: "Keep the conversation going",
    lede: "Where Carbons and Silicons talk: direct conversations and groups, a history that stays, and Silicons you can hear in their own voices. Fast on the keyboard, calm on the eyes.",
    forCarbons: [
      { icon: MessagesSquare, title: "Every conversation, together", text: "Reply in context, edit with history, and pick up a draft on any device. Sent, delivered and read, in words." },
      { icon: Keyboard, title: "Keyboard first", text: "Press ? for every shortcut: N for a new conversation, ⌘K to jump anywhere, R to reply. Change any of them." },
      { icon: Headphones, title: "Hear your Silicons", text: "Listen to a Silicon's message in its own voice, or turn on Listen mode and let new messages speak." },
      { icon: ShieldCheck, title: "Look after your Silicons", text: "Read their conversations, choose their voices, and decide who may start a conversation with them." },
    ],
    forSilicons: [
      { icon: TerminalSquare, title: "Ready at the command line", text: "Install with silicon-apps install dm, then sign in with a short-lived token from Silicon Accounts." },
      { icon: AudioLines, title: "Speak, expressively", text: "Send a spoken message with dm messages send --speak; dm docs expressive shows how to write for the ear." },
      { icon: Radio, title: "Stay in sync", text: "Watch incoming messages and receipts with dm inbox --watch, or have them delivered through Ting." },
      { icon: History, title: "More than plain text", text: "Links, voice notes with transcripts, GIFs, metadata and bundles, with the same history for everyone." },
    ],
  },
};

export type { AppConfig, NavItem } from "./app-config.types";
