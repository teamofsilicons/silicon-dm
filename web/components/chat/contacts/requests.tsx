"use client";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { AccountChip } from "@/components/foundation/account/account-chip";
import { ApiError } from "@/lib/errors";
import { dm } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import { requestsKey, useBook, useRequests } from "@/lib/chat/contacts";
import styles from "./people.module.css";

export async function answerRequest(id:string,accept:boolean,owner:string,client:QueryClient) {
  const result=await dm<{conversation_id?:string}>(`/api/v1/chat-requests/${encodeURIComponent(id)}`,{method:"PUT",body:{type:"chat_request.answer",data:{accept}}});
  await Promise.all([client.invalidateQueries({queryKey:requestsKey(owner)}),client.invalidateQueries({queryKey:chatKeys.conversations()})]);
  return result;
}

export function RequestsPage(){
  const book=useBook(),query=useRequests(book.owner),client=useQueryClient(),router=useRouter();
  const [error,setError]=useState<ApiError|null>(null),[busy,setBusy]=useState("");
  async function answer(id:string,accept:boolean){
    setBusy(id);setError(null);
    try{const result=await answerRequest(id,accept,book.owner,client);if(result.conversation_id) router.push(`/c/${encodeURIComponent(result.conversation_id)}`);}
    catch(e){setError(ApiError.from(e));} finally{setBusy("");}
  }
  return <div className={styles.page}><div className={styles.top}><Link href="/c">Chats</Link><Link href="/contacts">Contacts</Link></div><h1>Requests</h1>
    {(query.error || error) && <ErrorAlert title="Could not complete the request" error={error ?? ApiError.from(query.error)}/>}
    {query.isPending && <p role="status">Loading requests…</p>}
    {query.data && !query.data.items.length && <p>No pending chat requests.</p>}
    <ul className={styles.list}>{query.data?.items.map(r=>{const p=book.project(r.sender);return <li key={r.id}><AccountChip account={{...p,kind:p.type}}/>
      <div className={styles.actions}><Button disabled={!!busy} onClick={()=>void answer(r.id,true)}>Accept</Button>
      <Link href={`/contacts?person=${encodeURIComponent(p.id)}&request=${encodeURIComponent(r.id)}`}>Save contact and accept</Link>
      <Button variant="ghost" disabled={!!busy} onClick={()=>void answer(r.id,false)}>Decline</Button></div></li>;})}</ul></div>;
}

export function DirectSettings(){
  const {owner}=useBook(),client=useQueryClient(),key=["dm","direct-settings",owner];
  const query=useQuery({queryKey:key,queryFn:()=>dm<{contacts_only:boolean}>("/api/v1/direct-settings")});
  const [error,setError]=useState<ApiError|null>(null),[busy,setBusy]=useState(false);
  async function change(checked:boolean){
    setBusy(true);setError(null);
    try{const result=await dm("/api/v1/direct-settings",{method:"PUT",body:{type:"direct_settings",data:{contacts_only:checked}}});client.setQueryData(key,result);}
    catch(e){setError(ApiError.from(e));}finally{setBusy(false);}
  }
  return <div><label className={styles.inline}><input type="checkbox" disabled={!query.data || busy} checked={!!query.data?.contacts_only} onChange={e=>void change(e.target.checked)}/>Only contacts can DM me</label>
    <p>People outside your contacts will need your approval in <Link href="/requests">Requests</Link>.</p>
    {(error || query.error) && <ErrorAlert error={error ?? ApiError.from(query.error)} title="Could not update message settings"/>}</div>;
}
