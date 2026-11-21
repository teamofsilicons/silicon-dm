"use client";
import { useId, useState } from "react";
import { AccountChip } from "@/components/foundation/account/account-chip";
import { useBook, useExactPerson } from "@/lib/chat/contacts";
import type { AccountRef } from "@/lib/chat/types";
import { matchAccounts } from "./contacts";
import styles from "./account-picker.module.css";

interface Props {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  people: AccountRef[];
  allowEmail?: boolean;
  disabled?: boolean;
}
export function RecipientField({ id, label, value, onChange, people, allowEmail, disabled }: Props) {
  const list = useId(),
    [active, setActive] = useState(-1),
    [open, setOpen] = useState(false);
  const { items } = useBook();
  const text = value.trim().toLowerCase();
  const email =
    allowEmail && text.includes("@") ? items.find((c) => !c.blocked && c.email.toLowerCase() === text) : undefined;
  const known = people.find((p) => p.id === text || (email && p.uuid === email.uuid));
  const preview = useExactPerson(known?.id ?? value, known);
  const matches = matchAccounts(people, value, 6);
  const show = open && !!matches.length && !preview;
  function pick(person: AccountRef) {
    onChange(person.id);
    setOpen(false);
    setActive(-1);
  }
  return (
    <div className={styles.field}>
      <label className={styles.label} htmlFor={id}>
        {label}
      </label>
      <div className={styles.control} data-sq="surface">
        <input
          id={id}
          className={styles.input}
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={show}
          aria-controls={list}
          aria-activedescendant={show && active >= 0 ? `${list}-${active}` : undefined}
          required
          disabled={disabled}
          value={value}
          onChange={(e) => {
            onChange(e.target.value);
            setOpen(true);
            setActive(-1);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => setOpen(false)}
          placeholder={allowEmail ? "Name, account ID or email" : "Name or account ID"}
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="none"
          maxLength={254}
          onKeyDown={(e) => {
            if (e.key === "Escape" && show) {
              e.preventDefault();
              e.stopPropagation();
              setOpen(false);
            }
            if ((e.key === "ArrowDown" || e.key === "ArrowUp") && matches.length) {
              e.preventDefault();
              setOpen(true);
              setActive((active + (e.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length);
            }
            if (e.key === "Enter" && show && active >= 0) {
              e.preventDefault();
              pick(matches[active]!);
            }
          }}
        />
      </div>
      <div id={list} role="listbox" hidden={!show} className={styles.list} aria-label={`${label} suggestions`}>
        {show &&
          matches.map((p, i) => (
            <div
              key={p.uuid ?? p.id}
              role="option"
              id={`${list}-${i}`}
              aria-selected={active === i}
              className={styles.option}
              data-active={active === i ? "" : undefined}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pick(p)}
            >
              <AccountChip account={{ ...p, kind: p.type }} note={p.trust ? `Trust ${p.trust}` : undefined} />
            </div>
          ))}
      </div>
      {preview && <AccountChip account={{ ...preview, kind: preview.type }} variant="chip" />}
    </div>
  );
}
