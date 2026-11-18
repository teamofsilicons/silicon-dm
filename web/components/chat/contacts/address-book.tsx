"use client";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useState, type FormEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { AccountChip } from "@/components/foundation/account/account-chip";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { ApiError } from "@/lib/errors";
import { dm } from "@/lib/chat/api";
import { contactsKey, refOf, useBook, useExactPerson, type Contact, type ContactFields } from "@/lib/chat/contacts";
import { useKnownAccounts } from "../dialogs/use-known-accounts";
import { useStartConversation } from "../dialogs/open-conversation";
import { RecipientField } from "../dialogs/recipient-field";
import { matchAccounts, typedAccount } from "../dialogs/contacts";
import { answerRequest } from "./requests";
import styles from "./people.module.css";

const defaults: ContactFields = {
  name: "",
  trust: 1,
  note: "",
  photo_id: null,
  email: "",
  phone: "",
  birthday_month: null,
  birthday_day: null,
  birthday_year: null,
};
const enc = encodeURIComponent;
function Editor({
  peer,
  saved,
  requestId,
  onDone,
}: {
  peer: string;
  saved?: Contact;
  requestId: string | null;
  onDone: () => void;
}) {
  const [id, setId] = useState(peer),
    [name, setName] = useState(saved?.name),
    [trust, setTrust] = useState(saved?.trust ?? 1);
  const [note, setNote] = useState(saved?.note ?? ""),
    [file, setFile] = useState<File | null>(null),
    [removePhoto, setRemovePhoto] = useState(false);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState<ApiError | null>(null);
  const book = useBook(),
    known = useKnownAccounts(),
    client = useQueryClient(),
    router = useRouter();
  const preview = useExactPerson(
    id,
    known.find((p) => p.id === id),
  );
  const effectiveName = name ?? preview?.display_name ?? id;
  async function save(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      if (!typedAccount(id).ok || !effectiveName.trim()) throw new Error("Choose an account and enter a contact name.");
      const selected =
        saved ?? book.items.find((c) => c.account.id === id || (preview?.uuid && c.uuid === preview.uuid));
      if (!saved && selected) {
        const next = new URLSearchParams({ person: selected.account.id });
        if (requestId) next.set("request", requestId);
        router.replace(`/contacts?${next}`);
        return;
      }
      const current = selected ? await dm<Contact>(`/api/v1/contacts/${enc(selected.uuid)}`) : undefined;
      const data: ContactFields = { ...defaults };
      if (current)
        for (const key of Object.keys(defaults) as Array<keyof ContactFields>)
          Object.assign(data, { [key]: current[key] });
      if (!saved || name !== saved.name) data.name = effectiveName.trim();
      if (!saved || trust !== saved.trust) data.trust = trust;
      if (!saved || note !== saved.note) data.note = note;
      if (removePhoto) data.photo_id = null;
      if (file) {
        const form = new FormData();
        form.append("photo", file);
        data.photo_id = (await dm<{ id: string }>("/api/v1/contact-photos", { method: "POST", raw: form })).id;
      }
      await dm(current ? `/api/v1/contacts/${enc(current.uuid)}` : "/api/v1/contacts", {
        method: current ? "PUT" : "POST",
        body: { type: current ? "contact.update" : "contact.save", data: { ...data, id } },
      });
      await client.invalidateQueries({ queryKey: contactsKey(book.owner) });
      if (requestId) {
        const result = await answerRequest(requestId, true, book.owner, client);
        if (result.conversation_id) {
          router.push(`/c/${enc(result.conversation_id)}`);
          return;
        }
      }
      onDone();
    } catch (failure) {
      setError(ApiError.from(failure));
    } finally {
      setBusy(false);
    }
  }
  return (
    <form onSubmit={save} className={styles.form}>
      <h2>{saved ? "Edit contact" : "Add contact"}</h2>
      <RecipientField
        id="contact-peer"
        label="Account"
        value={id}
        onChange={setId}
        people={known}
        disabled={!!saved || busy}
      />
      <label>
        Contact name
        <input required maxLength={200} value={effectiveName} onChange={(e) => setName(e.target.value)} />
      </label>
      <label>
        Trust <span className={styles.muted}>1–255</span>
        <input
          type="number"
          min={1}
          max={255}
          required
          value={trust || ""}
          onChange={(e) => setTrust(Number(e.target.value))}
        />
      </label>
      <label className={styles.inline}>
        <input type="checkbox" checked={trust === 2} onChange={(e) => setTrust(e.target.checked ? 2 : 1)} />
        Trusted
      </label>
      <label>
        Notes
        <textarea rows={3} maxLength={10000} value={note} onChange={(e) => setNote(e.target.value)} />
      </label>
      <label>
        Contact photo
        <input
          type="file"
          accept="image/png,image/jpeg,image/webp"
          onChange={(e) => {
            setFile(e.target.files?.[0] ?? null);
            setRemovePhoto(false);
          }}
        />
      </label>
      {saved?.photo_id && !file && (
        <label className={styles.inline}>
          <input type="checkbox" checked={removePhoto} onChange={(e) => setRemovePhoto(e.target.checked)} />
          Use account photo
        </label>
      )}
      {error && <ErrorAlert error={error} title="Could not save contact" />}
      <div className={styles.actions}>
        <Button type="submit" loading={busy}>
          {requestId ? "Save and accept" : "Save contact"}
        </Button>
        <Button type="button" variant="ghost" onClick={onDone}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

export function AddressBookPage() {
  const book = useBook(),
    params = useSearchParams(),
    router = useRouter(),
    client = useQueryClient(),
    start = useStartConversation();
  const peer = params.get("person"),
    requestId = params.get("request"),
    [q, setQ] = useState(""),
    [error, setError] = useState<ApiError | null>(null),
    [confirm, setConfirm] = useState("");
  const selected = book.items.find((c) => c.account.id === peer || c.uuid === peer);
  const refs = book.items.map((c) => book.project(refOf(c.account))).sort((a, b) => (b.trust ?? 0) - (a.trust ?? 0));
  const matches = matchAccounts(refs, q, refs.length);
  async function remove(uuid: string) {
    try {
      await dm(`/api/v1/contacts/${enc(uuid)}`, { method: "DELETE" });
      await client.invalidateQueries({ queryKey: contactsKey(book.owner) });
      setConfirm("");
    } catch (e) {
      setError(ApiError.from(e));
    }
  }
  return (
    <div className={styles.page}>
      <div className={styles.top}>
        <Link href="/c">Chats</Link>
        <Link href="/requests">Requests</Link>
      </div>
      <header>
        <h1>Contacts</h1>
        <span className={styles.muted}>Your Silicon Contacts address book</span>
      </header>
      {book.error && <ErrorAlert error={ApiError.from(book.error)} title="Could not load Silicon Contacts" />}
      {peer !== null ? (
        book.loading ? (
          <p role="status">Loading contacts…</p>
        ) : (
          <Editor
            key={peer}
            peer={selected?.account.id ?? peer}
            saved={selected}
            requestId={requestId}
            onDone={() => router.replace("/contacts")}
          />
        )
      ) : (
        <>
          <div className={styles.toolbar}>
            <input
              aria-label="Search contacts"
              type="search"
              placeholder="Search contacts"
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
            <Link href="/contacts?person=">Add contact</Link>
          </div>
          {book.loading && <p role="status">Loading contacts…</p>}
          {!book.loading && !book.error && !matches.length && (
            <p>{q ? "No contacts match this search." : "No saved contacts yet."}</p>
          )}
          {error && <ErrorAlert title="Could not complete the action" error={error} />}
          <ul className={styles.list}>
            {matches.map((p) => (
              <li key={p.uuid}>
                <AccountChip account={{ ...p, kind: p.type }} note={`Trust ${p.trust}`} />
                <div className={styles.actions}>
                  <Button
                    variant="secondary"
                    onClick={() => void start([p.id]).catch((e) => setError(ApiError.from(e)))}
                  >
                    Chat
                  </Button>
                  <Link href={`/contacts?person=${enc(p.id)}`}>Edit</Link>
                  {confirm === p.uuid ? (
                    <>
                      <Button variant="ghost" onClick={() => void remove(p.uuid!)}>
                        Confirm removal
                      </Button>
                      <button onClick={() => setConfirm("")}>Cancel</button>
                    </>
                  ) : (
                    <button onClick={() => setConfirm(p.uuid!)}>Remove</button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
