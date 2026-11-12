"use client";

import { accountName } from "@/lib/chat/address";
import type { AccountRef } from "@/lib/chat/types";
import { AccountAvatar } from "../common/avatars";
import styles from "./composer.module.css";

export const mentionOptionId = (list: string, index: number) => `${list}-member-${index}`;

export function MentionMenu({ id, members, active, onChoose, onHover }: { id: string; members: AccountRef[]; active: number; onChoose: (index: number) => void; onHover: (index: number) => void }) {
  return <div className={styles.slashMenu} data-sq="surface">
    <div role="listbox" id={id} aria-label="Group members" className={styles.mentionList}>
      {members.map((member, index) => <div key={member.uuid ?? member.id} id={mentionOptionId(id, index)} role="option" aria-selected={index === active}
        className={styles.slashOption} data-sq="surface" onMouseDown={event => event.preventDefault()} onMouseMove={() => onHover(index)} onClick={() => onChoose(index)}>
        <AccountAvatar account={member} size="small" decorative />
        <span className={styles.mentionPerson}><span>{accountName(member)}</span><span className={styles.mentionId}>@{member.id}</span></span>
      </div>)}
    </div>
    <p className={styles.slashFoot} aria-hidden="true">↑↓ choose · Tab or Enter selects · Esc closes</p>
  </div>;
}
