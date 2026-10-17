"use client";

/**
 * Add a link (from the + menu or `/link`): one field above the message, like the reply bar. Enter adds the link to the
 * tray (it goes with the message as an attachment), Esc closes the field and gives the keyboard back to the message.
 * DM takes https links only; "example.com/x" is read as https://example.com/x.
 */
import { useId, useState } from "react";
import { Link2, X } from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { normalizeLink } from "./slash";
import styles from "./composer.module.css";

export function LinkBar({ prefill, onAdd, onClose }: { prefill: string; onAdd: (url: string) => void; onClose: () => void }) {
  const [value, setValue] = useState(prefill);
  const [error, setError] = useState<string | null>(null);
  const errorId = useId();
  const add = () => {
    const url = normalizeLink(value);
    if (!url) {
      setError(value.trim() ? "That is not an https link. DM sends only https:// links." : "Paste or type a link first.");
      return;
    }
    onAdd(url);
  };
  return (
    <div className={styles.linkBlock}>
      <form
        className={styles.linkBar}
        data-sq="surface"
        // The link is checked here ("example.com/x" is fine and becomes https), not by the browser's own URL rules.
        noValidate
        onSubmit={event => {
          event.preventDefault();
          add();
        }}
      >
        <Link2 size={16} strokeWidth={1.75} aria-hidden="true" />
        <input
          className={styles.linkField}
          type="url"
          inputMode="url"
          autoComplete="off"
          spellCheck={false}
          placeholder="https://…"
          aria-label="Link to add"
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          value={value}
          // The field opens because the Carbon asked to add a link: the keyboard goes to it.
          autoFocus
          onChange={event => {
            setValue(event.target.value);
            setError(null);
          }}
          onKeyDown={event => {
            if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              onClose();
            }
          }}
        />
        <Button type="submit" size="sm" variant="secondary">Add link</Button>
        <button type="button" className={styles.barClose} data-sq="surface" onClick={onClose} aria-label="Cancel adding a link">
          <X size={16} strokeWidth={1.9} aria-hidden="true" />
        </button>
      </form>
      {error ? <p id={errorId} className={styles.linkError} role="alert">{error}</p> : null}
    </div>
  );
}
