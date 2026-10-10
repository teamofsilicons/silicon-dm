import {apiFetch} from "@/lib/server/rsc";
import {Settings} from "@/components/product/settings";
import type {Envelope} from "@/components/product/common";
import type {Me} from "@/lib/product/types";
export default async function Route(){const me=await apiFetch<Envelope<Me>>("/api/v1/me");return <Settings me={me.data}/>;}
