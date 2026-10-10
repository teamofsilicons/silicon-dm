import {apiFetch} from "@/lib/server/rsc";
import {Thread} from "@/components/product/thread";
import type {Envelope} from "@/components/product/common";
import type {Me,Page,Conversation} from "@/lib/product/types";
export default async function Route({params,searchParams}:{params:Promise<{id:string}>;searchParams:Promise<{for?:string}>}){const {id:encodedId}=await params;const id=decodeURIComponent(encodedId);const query=await searchParams;const me=await apiFetch<Envelope<Me>>("/api/v1/me");const list=await apiFetch<Envelope<Page<Conversation>>>(`/api/v1/conversations?limit=100${query.for?`&for=${encodeURIComponent(query.for)}`:""}`);return <Thread me={me.data} id={id} view={query.for??""} conversation={list.data.items.find(c=>c.id===id)}/>;}
