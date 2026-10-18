"use client";

/**
 * An icon button the way the chat's bars use them: a 36 px squircle, the name a person would say as its label, a
 * tooltip with that name and the shortcut's keys (Kbd), and `aria-keyshortcuts` so assistive technology announces the
 * keys too. A control that is off says why in its tooltip and stays focusable (aria-disabled), so the reason is found
 * by keyboard as well.
 */
import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";
import { Tooltip } from "@/components/silicon-ui/tooltip/tooltip";
import { useShortcutKeys } from "@/lib/shortcuts/react";
import { ShortcutHint } from "../keyboard/keys";
import styles from "./common.module.css";

export interface IconActionProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label" | "disabled"> {
  label: string;
  icon: ReactNode;
  /** The registry shortcut that does the same (its keys show in the tooltip). */
  shortcut?: string;
  /** Off, with the reason (shown in the tooltip and read after the name). */
  unavailable?: string | null;
  pressed?: boolean;
  tone?: "default" | "danger";
  size?: "md" | "sm";
  tooltipSide?: "top" | "bottom" | "left" | "right";
}

export const IconAction = forwardRef<HTMLButtonElement, IconActionProps>(function IconAction({ label, icon, shortcut, unavailable, pressed, tone = "default", size = "md", tooltipSide = "bottom", className, onClick, ...rest }, ref) {
  const keys = useShortcutKeys(shortcut ?? "");
  const tip = (
    <span className={styles.tip}>
      <span>{unavailable ? `${label}: ${unavailable}` : label}</span>
      {shortcut && !unavailable ? <ShortcutHint id={shortcut} /> : null}
    </span>
  );
  return (
    <Tooltip content={tip} side={tooltipSide}>
      <button
        {...rest}
        ref={ref}
        type="button"
        data-sq="surface"
        data-tone={tone}
        data-size={size}
        className={[styles.iconAction, className].filter(Boolean).join(" ")}
        aria-label={unavailable ? `${label} (${unavailable})` : label}
        aria-keyshortcuts={!unavailable && shortcut ? keys.aria : undefined}
        aria-pressed={pressed}
        aria-disabled={unavailable ? true : undefined}
        onClick={event => {
          if (unavailable) {
            event.preventDefault();
            return;
          }
          onClick?.(event);
        }}
      >
        {icon}
      </button>
    </Tooltip>
  );
});
