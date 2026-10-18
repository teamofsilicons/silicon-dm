"use client";

/**
 * The details panel's place in the layout. A page that has details (a conversation) renders them with
 * <DetailsPortal>; the shell decides where they show: a 340 px panel beside the thread on wide screens (open or closed
 * as the Carbon left it), a drawer from the right on medium ones, a bottom sheet on phones. ⌘. and the ⓘ button toggle it.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { BottomSheet } from "@/components/silicon-ui/bottom-sheet/bottom-sheet";
import { Drawer, DrawerContent } from "@/components/silicon-ui/drawer/drawer";
import { updateChatSettings, useChatSettings } from "@/lib/chat/settings";
import { useShortcut } from "@/lib/shortcuts/react";
import { useLayoutMode, type LayoutMode } from "./use-layout-mode";
import styles from "./shell.module.css";

interface DetailsValue {
  open: boolean;
  setOpen: (open: boolean) => void;
  toggle: () => void;
  mode: LayoutMode;
  /** A page has details to show. */
  available: boolean;
  setAvailable: (available: boolean) => void;
  container: HTMLElement | null;
  setContainer: (element: HTMLElement | null) => void;
  title: string;
  setTitle: (title: string) => void;
}

const DetailsContext = createContext<DetailsValue | null>(null);

export function DetailsProvider({ children }: { children: ReactNode }) {
  const mode = useLayoutMode();
  const settings = useChatSettings();
  // Wide screens remember the panel; a drawer or a sheet opens only when asked.
  const [layerOpen, setLayerOpen] = useState(false);
  const [available, setAvailable] = useState(false);
  const [container, setContainer] = useState<HTMLElement | null>(null);
  const [title, setTitle] = useState("Details");
  const open = mode === "wide" ? settings.detailsOpen : layerOpen;
  const setOpen = useCallback((next: boolean) => {
    if (mode === "wide") updateChatSettings({ detailsOpen: next });
    else setLayerOpen(next);
  }, [mode]);
  const toggle = useCallback(() => setOpen(!open), [open, setOpen]);
  useShortcut("details.toggle", () => {
    if (!available) return false;
    toggle();
  });
  const value = useMemo(() => ({ open, setOpen, toggle, mode, available, setAvailable, container, setContainer, title, setTitle }), [open, setOpen, toggle, mode, available, container, title]);
  return <DetailsContext.Provider value={value}>{children}</DetailsContext.Provider>;
}

export function useDetails(): DetailsValue {
  const value = useContext(DetailsContext);
  if (!value) throw new Error("useDetails() needs <DetailsProvider> (components/chat/shell/chat-app.tsx).");
  return value;
}

/** Where the shell puts the details: the panel (wide) or a drawer or sheet (narrower). */
export function DetailsHost() {
  const { open, setOpen, mode, available, setContainer, title } = useDetails();
  const shown = open && available;
  if (mode === "wide") {
    return shown ? (
      <aside className={styles.details} aria-labelledby="details-title">
        <header className={styles.detailsHead}>
          <h2 id="details-title" className={styles.detailsTitle}>{title}</h2>
          <button type="button" data-sq="surface" className={styles.iconButton} onClick={() => setOpen(false)} aria-label="Close details">
            <X size={18} strokeWidth={1.75} aria-hidden="true" />
          </button>
        </header>
        <div className={styles.detailsBody} ref={setContainer} />
      </aside>
    ) : null;
  }
  if (mode === "medium") {
    return (
      <Drawer open={shown} onOpenChange={setOpen}>
        <DrawerContent side="right" title={title} className={styles.detailsDrawer}>
          <div ref={setContainer} />
        </DrawerContent>
      </Drawer>
    );
  }
  return (
    <BottomSheet open={shown} onOpenChange={setOpen} title={title} detents={[0.6, 0.92]}>
      <div ref={setContainer} />
    </BottomSheet>
  );
}

/** A page's details: shown wherever the shell puts them, while the page is mounted. */
export function DetailsPortal({ title, children }: { title: string; children: ReactNode }) {
  const { container, setAvailable, setTitle } = useDetails();
  useEffect(() => {
    setAvailable(true);
    return () => setAvailable(false);
  }, [setAvailable]);
  useEffect(() => setTitle(title), [setTitle, title]);
  return container ? createPortal(children, container) : null;
}
