export interface Actor{type:"carbon"|"silicon";id:string;uuid?:string;display_name?:string;deleted?:boolean}
export interface Me{account:Actor;custodian_of:Actor[];delivery:{mode?:string;ting_enabled?:boolean;sync_path?:string}}
export interface Message{"message-id":string;conversation_id:string;sender:Actor;message:string;attachments:string[];voice_transcript?:string;metadata?:Record<string,unknown>;created_at:string;delivered_at?:string;read_at?:string;deleted_at?:string;updated_at?:string;reply?:{"message-id":string;content?:{message:string}|null};history?:unknown[];bundle?:{id:string};via_app?:string}
export interface Group{name:string;description:string;version:number;owners:Actor[];members?:Actor[];pending_invitations?:Actor[]}
export interface Conversation{id:string;participants:Actor[];group?:Group;last_message?:Message;updated_at:string;superseded_by?:string}
export interface Page<T>{items:T[];next_cursor?:string}
export interface Draft{version:number;message_content?:string;attachments?:{permanent_url:string}[];voice_transcript?:string;metadata?:Record<string,unknown>;reply_to_message_id?:string}
export interface Invitation{conversation_id:string;name:string;description:string;invited_by?:Actor}
export interface AllowList{silicon:Actor;items:{account:Actor}[]}
export const title=(c:Conversation,me?:Actor)=>c.group?.name??c.participants.filter(p=>p.uuid!==me?.uuid).map(p=>p.display_name||p.id).join(", ");
