"use client";

/**
 * Choosing accounts (New conversation's To, New group's Invite, Add members, a Silicon's reach): a "To" field as in
 * a mail or chat app. Chosen accounts are chips (photo, name, id, Carbon or Silicon); the field below them takes a name
 * or an id. People you already talk to are listed under the field (in the dialogs at once, elsewhere as you type or
 * on ↓), filtered as you type; ↑/↓ move through them and Enter takes the one shown first. An id nobody is listed
 * under (c:… for a Carbon, si:… for a Silicon) is taken as typed, and the service says who it is (or why not) when
 * the form is sent. Backspace in an empty field takes the last chip back; Esc clears what was typed first, so the next
 * Esc closes the dialog.
 *
 * Silicon UI has a single-choice Combobox and a free-text TagInput but no multiple-choice account field with
 * suggestions, so this composes the kit's account chip (AccountChip) with the WAI-ARIA combobox pattern
 * (role=combobox, aria-activedescendant, a listbox) on the kit's tokens.
 *
 *   const picker = useRef<AccountPickerHandle>(null);
 *   <AccountPicker ref={picker} label="To" value={accounts} onChange={setAccounts} suggestions={known} exclude={[me.account]} />
 *   const { accounts, problem } = picker.current!.commit();   // adds a typed id that was not entered yet
 */
import { forwardRef, useId, useImperativeHandle, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { Plus } from "lucide-react";
import { Avatar } from "@/components/silicon-ui/avatar/avatar";
import { motionTokens } from "@/components/silicon-ui/lib/motion-tokens";
import { AccountChip, KindMarker } from "@/components/foundation/account/account-chip";
import { accountName } from "@/lib/chat/address";
import type { AccountRef } from "@/lib/chat/types";
import { accountKey, isListed, keySet, matchAccounts, typedAccount } from "./contacts";
import styles from "./account-picker.module.css";

export interface AccountPickerHandle {
  focus: () => void;
  /** Adds what is typed (when it is an id) and returns the chosen accounts, or the reason the typed text is not an id. */
  commit: () => { accounts: AccountRef[]; problem: string | null };
}

export interface AccountPickerProps {
  label: string;
  /** One line under the field: what the accounts are chosen for. */
  description?: string;
  value: AccountRef[];
  onChange: (accounts: AccountRef[]) => void;
  /** People to offer (the ones you talk to, most recent first). */
  suggestions: AccountRef[];
  /** Accounts never offered nor taken (you, the members already in a group). */
  exclude?: AccountRef[];
  /** Why an excluded account cannot be chosen ("c:ada is you."), by its id. */
  excludedReason?: (account: AccountRef) => string;
  /** Chosen ids the service refused (its words are shown with the form's error): their chips show it too. */
  flagged?: string[];
  max?: number;
  /** How many people to list at once (default 5). */
  listSize?: number;
  /** List people while nothing is typed (dialogs, where choosing is the point); otherwise only as you type. */
  showRecent?: boolean;
  placeholder?: string;
  id?: string;
  disabled?: boolean;
}

type Option = { kind: "account"; account: AccountRef } | { kind: "typed"; account: AccountRef };

export const AccountPicker = forwardRef<AccountPickerHandle, AccountPickerProps>(function AccountPicker(
  { label, description, value, onChange, suggestions, exclude = [], excludedReason, flagged = [], max = 50, listSize = 5, showRecent = true, placeholder = "A name, or an id like c:ada or si:scout", id, disabled },
  ref,
) {
  const generated = useId();
  const inputId = id ?? `picker-${generated}`;
  const listId = `${inputId}-list`;
  const hintId = `${inputId}-hint`;
  const problemId = `${inputId}-problem`;
  const reduced = useReducedMotion() ?? false;
  const input = useRef<HTMLInputElement>(null);
  const [text, setText] = useState("");
  const [active, setActive] = useState(0);
  // The list "opens" for ↑/↓ while the field is empty; typing always opens it.
  const [browsing, setBrowsing] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [notice, setNotice] = useState("");

  const chosen = useMemo(() => keySet(value), [value]);
  const excluded = useMemo(() => keySet(exclude), [exclude]);
  const offer = useMemo(() => suggestions.filter(account => !isListed(account, chosen) && !isListed(account, excluded)), [suggestions, chosen, excluded]);

  const options = useMemo<Option[]>(() => {
    // Without `showRecent` the people show as you type, or when ↓ asks for them in the empty field.
    if (!text.trim() && !showRecent && !browsing) return [];
    const matches = matchAccounts(offer, text, listSize).map(account => ({ kind: "account" as const, account }));
    const typed = text.trim() ? typedAccount(text) : null;
    if (!typed?.ok || matches.some(option => option.account.id === typed.account.id) || isListed(typed.account, chosen) || isListed(typed.account, excluded)) return matches;
    // An id nobody in the list has is offered as typed: first when it is a whole new id, after the people whose ids
    // start with it while it may still be the start of one of theirs ("c:dmc-" → Bo, Cy… first).
    const option = { kind: "typed" as const, account: typed.account };
    const prefix = offer.some(account => account.id.startsWith(typed.account.id));
    return prefix ? [...matches.slice(0, listSize), option] : [option, ...matches].slice(0, listSize + 1);
  }, [offer, text, listSize, showRecent, browsing, chosen, excluded]);

  const expanded = !disabled && options.length > 0 && (!!text.trim() || browsing);
  // Text that is neither someone listed nor an id (an id already chosen or left out says so when it is added).
  const nobody = !disabled && !!text.trim() && !options.length && !typedAccount(text).ok;
  const activeIndex = expanded ? Math.min(active, options.length - 1) : -1;
  const optionId = (index: number) => `${inputId}-option-${index}`;
  const say = (message: string) => setNotice(previous => (previous === message ? `${message} ` : message));

  /** Adds an account; returns why it could not be added, or null. */
  function add(account: AccountRef): string | null {
    if (isListed(account, chosen)) {
      setText("");
      say(`${account.id} is already chosen.`);
      return null;
    }
    const refusal = isListed(account, excluded) ? excludedReason?.(account) ?? `${account.id} cannot be chosen here.` : value.length >= max ? `At most ${max} accounts at once.` : null;
    if (refusal) {
      setProblem(refusal);
      return refusal;
    }
    onChange([...value, account]);
    setText("");
    setActive(0);
    setBrowsing(false);
    setProblem(null);
    say(`Added ${accountName(account)}${account.display_name ? `, ${account.id}` : ""}.`);
    return null;
  }

  function remove(account: AccountRef) {
    onChange(value.filter(item => accountKey(item) !== accountKey(account)));
    say(`Removed ${account.id}.`);
    input.current?.focus();
  }

  /** What is typed, as an account to add: the first option, an exact id, or the reason it is neither. */
  function fromText(): { account: AccountRef } | { problem: string } | null {
    if (!text.trim()) return null;
    if (activeIndex >= 0 && options[activeIndex]) return { account: options[activeIndex]!.account };
    const typed = typedAccount(text);
    if (typed.ok) return { account: typed.account };
    const matches = matchAccounts(offer, text, 1);
    if (matches[0]) return { account: matches[0] };
    return { problem: typed.message };
  }

  useImperativeHandle(ref, () => ({
    focus: () => input.current?.focus(),
    commit: () => {
      const typed = fromText();
      if (!typed) return { accounts: value, problem: null };
      if ("problem" in typed) {
        setProblem(typed.problem);
        return { accounts: value, problem: typed.problem };
      }
      if (isListed(typed.account, chosen)) {
        setText("");
        return { accounts: value, problem: null };
      }
      const refusal = add(typed.account);
      return refusal ? { accounts: value, problem: refusal } : { accounts: [...value, typed.account], problem: null };
    },
  }));

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      // The people you talk to are there for ↓ even before anything is typed.
      const count = options.length || (!text.trim() ? Math.min(offer.length, listSize) : 0);
      if (!count) return;
      event.preventDefault();
      if (!expanded) {
        setBrowsing(true);
        setActive(event.key === "ArrowDown" ? 0 : count - 1);
        return;
      }
      setActive((activeIndex + (event.key === "ArrowDown" ? 1 : -1) + options.length) % options.length);
      return;
    }
    if (event.key === "Enter") {
      // Nothing typed: Enter belongs to the form (it sends it).
      if (!text.trim() && !expanded) return;
      event.preventDefault();
      const typed = expanded && options[activeIndex] ? { account: options[activeIndex]!.account } : fromText();
      if (!typed) return;
      if ("problem" in typed) setProblem(typed.problem);
      else void add(typed.account);
      return;
    }
    if (event.key === "Escape") {
      if (text || browsing) {
        // Esc clears what was typed (or closes the list) first; the next Esc closes the dialog.
        event.preventDefault();
        event.stopPropagation();
        setText("");
        setBrowsing(false);
        setProblem(null);
      }
      return;
    }
    if ((event.key === "," || event.key === ";") && text.trim()) {
      event.preventDefault();
      const typed = fromText();
      if (typed && "account" in typed) void add(typed.account);
      else if (typed) setProblem(typed.problem);
      return;
    }
    if (event.key === "Backspace" && !text && value.length) {
      event.preventDefault();
      remove(value[value.length - 1]!);
    }
  }

  // Several ids pasted at once ("c:ada, si:scout c:grace") are added one by one; what is not an id stays typed.
  function onPaste(pasted: string): boolean {
    const parts = pasted.split(/[\s,;]+/).filter(Boolean);
    if (parts.length < 2) return false;
    const next = [...value];
    const left: string[] = [];
    const keys = keySet(next);
    for (const part of parts) {
      const typed = typedAccount(part);
      if (!typed.ok || isListed(typed.account, excluded)) left.push(part);
      else if (!isListed(typed.account, keys) && next.length < max) {
        next.push(typed.account);
        keys.add(typed.account.id);
      }
    }
    onChange(next);
    setText(left.join(" "));
    say(`Added ${next.length - value.length} account${next.length - value.length === 1 ? "" : "s"}.`);
    return true;
  }

  const described = [description ? hintId : null, problem ? problemId : null].filter(Boolean).join(" ") || undefined;

  return (
    <div className={styles.field}>
      <label htmlFor={inputId} className={styles.label}>{label}</label>
      <div className={styles.control} data-sq="surface" data-invalid={problem ? "" : undefined} data-disabled={disabled ? "" : undefined}
        onPointerDown={event => {
          if (event.target === event.currentTarget) {
            event.preventDefault();
            input.current?.focus();
          }
        }}>
        {value.length ? (
          <ul className={styles.chips} role="list" aria-label={`${label}: ${value.length} account${value.length === 1 ? "" : "s"}`}>
            <AnimatePresence initial={false} mode="popLayout">
              {value.map(account => (
                <motion.li key={accountKey(account)} className={styles.chip} layout={reduced ? false : "position"}
                  initial={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.92, filter: `blur(${motionTokens.blur.soft}px)` }}
                  animate={{ opacity: 1, scale: 1, filter: "blur(0px)" }}
                  exit={reduced ? { opacity: 0, transition: { duration: 0 } } : { opacity: 0, scale: 0.92, transition: { duration: motionTokens.duration.instant } }}
                  transition={reduced ? { duration: motionTokens.duration.instant } : { ...motionTokens.spring.morph, opacity: { duration: motionTokens.duration.fast } }}>
                  <AccountChip account={{ id: account.id, uuid: account.uuid, kind: account.type, display_name: account.display_name, pfp_url: account.pfp_url }} variant="chip"
                    state={flagged.includes(account.id) ? "problem" : undefined} onRemove={disabled ? undefined : () => remove(account)} removeLabel={`Remove ${accountName(account)}${account.display_name ? ` (${account.id})` : ""}`} />
                </motion.li>
              ))}
            </AnimatePresence>
          </ul>
        ) : null}
        <input
          ref={input}
          id={inputId}
          className={styles.input}
          value={text}
          disabled={disabled}
          placeholder={value.length ? "Add another" : placeholder}
          role="combobox"
          aria-expanded={expanded}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={activeIndex >= 0 ? optionId(activeIndex) : undefined}
          aria-invalid={problem ? true : undefined}
          aria-describedby={described}
          // While something is typed, Esc clears it before it closes the dialog or sheet around the field
          // (Silicon UI's layers leave an Esc inside a [data-editing] control to the control: lib/escape.ts).
          data-editing={text ? "" : undefined}
          autoComplete="off"
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          enterKeyHint="done"
          onChange={event => {
            setText(event.currentTarget.value);
            setActive(0);
            if (problem) setProblem(null);
          }}
          onKeyDown={onKeyDown}
          onPaste={event => {
            if (onPaste(event.clipboardData.getData("text"))) event.preventDefault();
          }}
          onBlur={() => setBrowsing(false)}
        />
      </div>
      {problem ? <p id={problemId} className={styles.problem} role="alert">{problem}</p> : null}
      {description ? <p id={hintId} className={styles.hint}>{description}</p> : null}
      <div id={listId} role="listbox" aria-label={text.trim() ? `Accounts matching ${text.trim()}` : "People you talk to"} className={styles.list} hidden={!options.length || disabled}>
        {!text.trim() && options.length ? <div className={styles.listLabel} aria-hidden="true">People you talk to</div> : null}
        {options.map((option, index) => (
          <div key={`${option.kind}:${accountKey(option.account)}`} id={optionId(index)} role="option" aria-selected={index === activeIndex}
            className={styles.option} data-sq="surface" data-active={index === activeIndex ? "" : undefined}
            onPointerDown={event => event.preventDefault()}
            onPointerMove={() => { if (expanded && index !== activeIndex) setActive(index); }}
            onClick={() => { void add(option.account); input.current?.focus(); }}>
            {/* The way the details panel lists a person: the name, then the id and Carbon or Silicon under it. */}
            {option.kind === "typed" ? (
              <span className={styles.typedMark} data-sq="surface" aria-hidden="true"><Plus size={14} strokeWidth={2} /></span>
            ) : (
              <Avatar name={accountName(option.account).replace(/^(c|si):/, "")} src={option.account.pfp_url} size="sm" className={styles.optionAvatar} aria-hidden="true" />
            )}
            <span className={styles.optionText}>
              <span className={styles.optionName}>{option.kind === "typed" ? `Add ${option.account.id}` : accountName(option.account)}</span>
              <span className={styles.optionMeta}>
                {option.kind === "account" && option.account.display_name ? <span className={styles.optionId}>{option.account.id}</span> : null}
                <KindMarker kind={option.account.type} size="xs" />
              </span>
            </span>
          </div>
        ))}
      </div>
      {/* Always there (empty, it takes no room), so the words are heard when they come: a query that matches no one
          says so, and how to name someone who is not in the list yet. */}
      <p className={styles.listEmpty} aria-live="polite">{nobody ? `No one you talk to matches “${text.trim()}”. Type an id such as c:ada for a Carbon or si:scout for a Silicon.` : ""}</p>
      <span className="sr-only" aria-live="polite">{notice}</span>
    </div>
  );
});
