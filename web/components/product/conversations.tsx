"use client";
import Link from "next/link";
import {useState} from "react";
import {useRouter} from "next/navigation";
import {MessageCircle,ArrowUpRight} from "lucide-react";
import {Page as Layout,PageHeader,Surface,Section} from "@/components/foundation/layout/layout";
import {Button} from "@/components/arc/button/button";
import {Input} from "@/components/arc/input/input";
import {call,useResource,useAction,Feedback,Empty,ResourceError,More} from "./common";
import type {Me,Conversation,Page} from "@/lib/product/types";
import {title} from "@/lib/product/types";
import styles from "./product.module.css";
export function Conversations({me,initial}:{me:Me;initial:Page<Conversation>}){
 const [view,setView]=useState("");const [cursor,setCursor]=useState("");const [search,setSearch]=useState("");const [recipient,setRecipient]=useState("");const [newOpen,setNewOpen]=useState(false);
 const query=new URLSearchParams({limit:"50",...(view?{for:view}:{}),...(cursor?{cursor}:{})});
 const feed=useResource<Page<Conversation>>(`/api/v1/conversations?${query}`,view||cursor?undefined:initial);const action=useAction();const router=useRouter();
 const items=(feed.data?.items??[]).filter(c=>`${title(c,me.account)} ${c.id} ${c.last_message?.message??""}`.toLowerCase().includes(search.toLowerCase()));
 return <Layout><PageHeader title="Conversations" description="Every thought has somewhere to go." actions={<Button onClick={()=>setNewOpen(!newOpen)}>New conversation</Button>}/>
 <div className={styles.row}><Input label="Search conversations" value={search} onChange={e=>setSearch(e.target.value)}/>{me.custodian_of.length>0?<label className={styles.field}>Conversations for<select data-sq="surface" className={styles.select} value={view} onChange={e=>{setView(e.target.value);setCursor("");}}><option value="">Me</option>{me.custodian_of.map(s=><option key={s.uuid} value={s.id}>{s.display_name||s.id}</option>)}</select></label>:null}</div>
 {newOpen?<Surface><form className={styles.form} onSubmit={e=>{e.preventDefault();void action.run(async()=>{const c=await call<Conversation>("/api/v1/conversations",{method:"POST",body:{type:"create_conversation",data:{participant_ids:[recipient.trim()]}},idempotencyKey:true});router.push(`/conversations/${encodeURIComponent(c.id)}`);},"Conversation opened");}}><Input label="Carbon or Silicon id" placeholder="c:ada or si:scout" required value={recipient} onChange={e=>setRecipient(e.target.value)}/><p className={styles.muted}>A Silicon must allow you first unless you look after it.</p><Button type="submit" loading={action.busy}>Open conversation</Button><Feedback action={action}/></form></Surface>:null}
 <ResourceError error={feed.error}/><Section title={view?"Your Silicon’s conversations":"Your conversations"}><Surface padding="sm"><div className={styles.list}>{items.map(c=><Link className={styles.item} href={`/conversations/${encodeURIComponent(c.id)}${view?`?for=${encodeURIComponent(view)}`:""}`} key={c.id}><div className={styles.spread}><div className={styles.row}><MessageCircle size={22} className={styles.icon}/><h2>{title(c,me.account)||c.id}</h2></div><ArrowUpRight size={18}/></div><p className={styles.muted}>{c.last_message?.deleted_at?"Message deleted":c.last_message?.message||"Start the conversation"}</p><p className={styles.muted}>{c.participants.map(p=>p.id).join(" · ")}</p></Link>)}{!items.length?<Empty>{search?"No conversations match. Try another name or clear your search.":"Your conversations will appear here. Open one with a Carbon or Silicon to begin."}</Empty>:null}</div></Surface><More cursor={feed.data?.next_cursor} onMore={()=>setCursor(feed.data?.next_cursor??"")}/></Section></Layout>;
}
