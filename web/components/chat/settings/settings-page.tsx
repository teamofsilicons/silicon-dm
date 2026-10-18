"use client";

/**
 * Settings (G S): Voice, Listening, Sending, Keyboard, Notifications, Account and Report a problem, in one calm
 * column. On wide screens Silicon UI's nav list sits beside it and follows the reading position; narrower, a line of
 * the sections' names sits under the title (no sideways scrolling). Every section has an address (`/settings#keyboard`),
 * so the palette, the keyboard (G V) and links go straight to one. An address that names no section (`#reach`, which
 * DM 0.17.0 and older linked to) opens the page at its top.
 */
import Link from "next/link";
import { useCallback, useEffect, useMemo, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { ArrowLeft, Bell, Bug, CircleUserRound, Headphones, Keyboard, Send, Volume2 } from "lucide-react";
import { NavItem, NavList } from "@/components/silicon-ui/nav-list/nav-list";
import { inboxHref } from "@/lib/chat/address";
import { useTitle } from "@/lib/chat/title";
import { ListeningSettings, VoiceSettings } from "../voice/settings-parts";
import { useChat } from "../shell/chat-context";
import { KeyboardSection } from "./keyboard-section";
import { AccountSection, NotificationsSection, ReportSection, SendingSection } from "./sections";
import styles from "./settings.module.css";

interface SectionSpec {
  id: string;
  title: string;
  /** The nav's shorter name, when the title is long. */
  short?: string;
  description: string;
  icon: ReactNode;
  body: ReactNode;
}

const SCROLL = "settings-scroll";
/** A section is the current one once its top is this far from the top of the column. */
const READ_LINE = 140;

/** Follows the reading position (the section whose top passed the read line), and jumps to a section by its id. */
function useSections(ids: string[]): { current: string; jump: (id: string, focus: boolean, instant?: boolean) => void } {
  const [current, setCurrent] = useState(ids[0] ?? "");
  // After a jump the column is still moving; the spy waits until it has arrived.
  const settling = useRef(0);
  // The page lands at its #section once; later changes (a section appearing) never move the reader again.
  const landed = useRef(false);

  const jump = useCallback((id: string, focus: boolean, instant = false) => {
    const root = document.getElementById(SCROLL);
    const node = document.getElementById(id);
    if (!root || !node) return;
    const reduced = instant || window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    settling.current = performance.now() + (reduced ? 50 : 900);
    setCurrent(id);
    // The column scrolls on its own (scrollIntoView would also try every scrollable ancestor).
    root.scrollTo({ top: node.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop - 16, behavior: reduced ? "auto" : "smooth" });
    if (focus) node.querySelector<HTMLElement>("h2")?.focus({ preventScroll: true });
  }, []);

  useEffect(() => {
    const root = document.getElementById(SCROLL);
    if (!root) return;
    let frame = 0;
    const measure = () => {
      if (performance.now() < settling.current) return;
      const top = root.getBoundingClientRect().top;
      let next = ids[0] ?? "";
      for (const id of ids) {
        const node = document.getElementById(id);
        if (node && node.getBoundingClientRect().top - top <= READ_LINE) next = id;
      }
      // At the very end the last sections cannot reach the read line: the last one shown wins.
      if (root.scrollTop > 0 && root.scrollTop + root.clientHeight >= root.scrollHeight - 2) {
        const last = [...ids].reverse().find(id => {
          const node = document.getElementById(id);
          return node && node.getBoundingClientRect().top - top < root.clientHeight * 0.6;
        });
        if (last) next = last;
      }
      setCurrent(next);
    };
    const onScroll = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    const fromHash = (event?: Event) => {
      const hash = decodeURIComponent(window.location.hash.slice(1));
      if (!ids.includes(hash) || (!event && landed.current)) return;
      landed.current = true;
      // Opening the page at a section lands there at once; a link on the page glides to it.
      jump(hash, false, !event);
    };
    // Opened at a section (`/settings#keyboard`): it is there at once. Sections above it that grow while their data
    // loads do not move it: the browser keeps what is on screen in place (scroll anchoring). An address naming no
    // section stays at the top.
    fromHash();
    root.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("hashchange", fromHash);
    return () => {
      cancelAnimationFrame(frame);
      root.removeEventListener("scroll", onScroll);
      window.removeEventListener("hashchange", fromHash);
    };
  }, [ids, jump]);

  return { current, jump };
}

export function SettingsPage() {
  const { view } = useChat();
  useTitle("Settings · DM");
  const icon = (Icon: typeof Bell) => <Icon size={16} strokeWidth={1.75} />;
  const sections: SectionSpec[] = [
    { id: "voice", title: "Voice", description: "How your Silicons sound when you listen to them, and how you sound.", icon: icon(Volume2), body: <VoiceSettings /> },
    { id: "listening", title: "Listening", description: "Listen to messages, and Listen mode.", icon: icon(Headphones), body: <ListeningSettings /> },
    { id: "sending", title: "Sending", description: "Which keys send a message.", icon: icon(Send), body: <SendingSection /> },
    { id: "keyboard", title: "Keyboard", description: "Every shortcut, and your own keys for them.", icon: icon(Keyboard), body: <KeyboardSection /> },
    { id: "notifications", title: "Notifications", description: "When this browser tells you about new messages.", icon: icon(Bell), body: <NotificationsSection /> },
    { id: "account", title: "Account", description: "Who you are signed in as.", icon: icon(CircleUserRound), body: <AccountSection /> },
    { id: "report", title: "Report a problem", short: "Report", description: "Tell the people who build DM what went wrong.", icon: icon(Bug), body: <ReportSection /> },
  ];
  const key = sections.map(section => section.id).join(" ");
  // The ids change only with the list of sections (the key), not with each render.
  const ids = useMemo(() => key.split(" "), [key]);
  const { current, jump } = useSections(ids);

  // A link to a section moves the reading position (and the keyboard, so Tab continues there).
  const onSectionLink = (id: string) => (event: MouseEvent<HTMLElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    window.history.replaceState(window.history.state, "", `#${id}`);
    jump(id, event.detail === 0);
  };

  return (
    <div className={styles.page} id={SCROLL}>
      <div className={styles.layout}>
        <header className={styles.header}>
          <Link href={inboxHref(view)} className={styles.back}><ArrowLeft size={16} strokeWidth={1.75} aria-hidden="true" /> Conversations</Link>
          <h1 className={styles.title}>Settings</h1>
          <p className={styles.lede}>For this browser, and for the account you are signed in with.</p>
          <nav className={styles.jumps} aria-label="Settings sections">
            <ul role="list">
              {sections.map(section => (
                <li key={section.id}>
                  <a href={`#${section.id}`} aria-current={current === section.id ? "location" : undefined} onClick={onSectionLink(section.id)}>{section.short ?? section.title}</a>
                </li>
              ))}
            </ul>
          </nav>
        </header>
        <div className={styles.toc} onClickCapture={event => {
          const link = (event.target as HTMLElement).closest<HTMLAnchorElement>("a[href^='#']");
          if (link) onSectionLink(link.getAttribute("href")!.slice(1))(event);
        }}>
          <NavList label="Settings sections">
            {sections.map(section => <NavItem key={section.id} href={`#${section.id}`} icon={section.icon} active={current === section.id}>{section.title}</NavItem>)}
          </NavList>
        </div>
        <div className={styles.sections}>
          {sections.map(section => (
            <section key={section.id} id={section.id} className={styles.section} aria-labelledby={`${section.id}-title`}>
              <div className={styles.sectionHead}>
                <h2 id={`${section.id}-title`} className={styles.sectionTitle} tabIndex={-1}>{section.title}</h2>
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
