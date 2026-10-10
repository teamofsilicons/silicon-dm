import {apiFetch,tryApiFetch} from "@/lib/server/rsc";
import {ErrorAlert} from "@/components/foundation/feedback/error-alert";
import {Conversations} from "@/components/product/conversations";
import type {Envelope} from "@/components/product/common";
import type {Me,Page,Conversation} from "@/lib/product/types";
export default async function Route(){const [me,list]=await Promise.all([apiFetch<Envelope<Me>>("/api/v1/me"),tryApiFetch<Envelope<Page<Conversation>>>("/api/v1/conversations?limit=50")]);if(list.error)return <ErrorAlert error={list.error}/>;return <Conversations me={me.data} initial={list.data.data}/>;}
