"use client";

/**
 * Settings (G S): Voice, Listening, Sending, Keyboard, Notifications, your Silicons' reach, Account and Report a
 * problem, one calm column with a list of the sections beside it on wide screens. Each section has an address
 * (`/settings#keyboard`), so the palette and the keyboard (G V) go straight to one.
 */
import Link from "next/link";
import { useEffect, useState, type ReactNode } from "react";
import { ArrowLeft, Bell, Bug, CircleUserRound, Headphones, Keyboard, Send, ShieldCheck, Volume2 } from "lucide-react";
import { NavItem, NavList } from "@/components/silicon-ui/nav-list/nav-list";
import { useTitle } from "@/lib/chat/title";
import { ListeningSettings, VoiceSettings } from "../voice/settings-parts";
import { useChat } from "../shell/chat-context";
import { KeyboardSection } from "./keyboard-section";
import { AccountSection, NotificationsSection, ReachSection, ReportSection, SendingSection } from "./sections";
import styles from "./settings.module.css";

interface SectionSpec {
  id: string;
  title: string;
  description: string;
  icon: ReactNode;
  body: ReactNode;
}

function useCurrentSection(ids: string[]): string {
  const [current, setCurrent] = useState(ids[0] ?? "");
  useEffect(() => {
    const root = document.getElementById("settings-scroll");
    // Scrolls the settings' own column (scrollIntoView would also move every scrollable ancestor).
    const fromHash = () => {
      const hash = window.location.hash.slice(1);
      const node = ids.includes(hash) ? document.getElementById(hash) : null;
      if (!node || !root) return;
      setCurrent(hash);
      root.scrollTo({ top: node.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop - 8 });
    };
    fromHash();
    window.addEventListener("hashchange", fromHash);
    const observer = new IntersectionObserver(entries => {
      const visible = entries.filter(entry => entry.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
      if (visible) setCurrent(visible.target.id);
    }, { root, rootMargin: "0px 0px -65% 0px" });
    for (const id of ids) {
      const node = document.getElementById(id);
      if (node) observer.observe(node);
    }
    return () => {
      window.removeEventListener("hashchange", fromHash);
      observer.disconnect();
    };
  }, [ids]);
  return current;
}

export function SettingsPage() {
  const { me } = useChat();
  useTitle("Settings · DM");
  const sections: SectionSpec[] = [
    { id: "voice", title: "Voice", description: "How your Silicons sound when you listen to them, and how you sound.", icon: <Volume2 size={16} strokeWidth={1.75} />, body: <VoiceSettings /> },
    { id: "listening", title: "Listening", description: "Listen to messages and Listen mode.", icon: <Headphones size={16} strokeWidth={1.75} />, body: <ListeningSettings /> },
    { id: "sending", title: "Sending", description: "Which keys send a message.", icon: <Send size={16} strokeWidth={1.75} />, body: <SendingSection /> },
    { id: "keyboard", title: "Keyboard", description: "Every shortcut, and your own keys for them.", icon: <Keyboard size={16} strokeWidth={1.75} />, body: <KeyboardSection /> },
    { id: "notifications", title: "Notifications", description: "When this browser tells you about new messages.", icon: <Bell size={16} strokeWidth={1.75} />, body: <NotificationsSection /> },
    ...(me.custodian_of.length ? [{ id: "reach", title: "Your Silicons' reach", description: "Who may start a conversation with a Silicon you look after, besides you and your other Silicons.", icon: <ShieldCheck size={16} strokeWidth={1.75} />, body: <ReachSection /> }] : []),
    { id: "account", title: "Account", description: "Who you are signed in as.", icon: <CircleUserRound size={16} strokeWidth={1.75} />, body: <AccountSection /> },
    { id: "report", title: "Report a problem", description: "Tell the people who build DM what went wrong.", icon: <Bug size={16} strokeWidth={1.75} />, body: <ReportSection /> },
  ];
  const ids = sections.map(section => section.id);
  const current = useCurrentSection(ids);

  return (
    <div className={styles.page} id="settings-scroll">
      <div className={styles.layout}>
        <header className={styles.header}>
          <Link href="/c" className={styles.back}><ArrowLeft size={16} strokeWidth={1.75} aria-hidden="true" /> Conversations</Link>
          <h1 className={styles.title}>Settings</h1>
          <p className={styles.lede}>For this browser, and for the account you are signed in with.</p>
        </header>
        <div className={styles.toc}>
          <NavList label="Settings sections">
            {sections.map(section => <NavItem key={section.id} href={`#${section.id}`} icon={section.icon} active={current === section.id}>{section.title}</NavItem>)}
          </NavList>
        </div>
        <div className={styles.sections}>
          {sections.map(section => (
            <section key={section.id} id={section.id} className={styles.section} aria-labelledby={`${section.id}-title`}>
              <div className={styles.sectionHead}>
                <h2 id={`${section.id}-title`} className={styles.sectionTitle}>{section.title}</h2>
                <p className={styles.sectionDescription}>{section.description}</p>
              </div>
              {section.body}
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
