import {apiFetch} from "@/lib/server/rsc";
import {GroupDetail} from "@/components/product/groups";
import type {Envelope} from "@/components/product/common";
import type {Me,Conversation} from "@/lib/product/types";
export default async function Route({params}:{params:Promise<{id:string}>}){const {id:encodedId}=await params;const id=decodeURIComponent(encodedId);const [me,group]=await Promise.all([apiFetch<Envelope<Me>>("/api/v1/me"),apiFetch<Envelope<Conversation>>(`/api/v1/groups/${encodeURIComponent(id)}`,{notFound:true})]);return <GroupDetail id={id} me={me.data} initial={group.data}/>;}
