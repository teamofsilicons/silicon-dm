/** Public DM wire models. Credentials deliberately have no persistent model. */
export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json };
export type Metadata = Record<string, Json>;
export type Actor = { type: "carbon" | "silicon"; id: string };
export type MessageStatus =
  | "waiting"
  | "sent"
  | "delivered"
  | "read"
  | "failed";
export type ReceiptStatus = "delivered" | "read";
export type Activity =
  | "typing"
  | "recording_voice"
  | "transcribing_voice"
  | "uploading_file"
  | "searching_gifs";
export interface Attachment {
  permanent_url: string;
  name?: string;
  content_type?: string;
  size?: number;
}
export interface VoiceAttachment extends Attachment {
  duration_milliseconds: number | null;
}
export interface Gif {
  provider_id: string;
  url: string;
  preview_url?: string;
  title?: string;
}
export interface MessageCreate {
  text?: string | null;
  metadata?: Metadata;
  sender_id?: string;
  reply_to_message_id?: string | null;
  attachments?: Attachment[];
  voice?: VoiceAttachment | null;
  voice_transcript?: string | null;
  gif?: Gif | null;
}
export interface Message extends Omit<MessageCreate, "sender_id"> {
  id: string;
  conversation_id: string;
  sender: Actor;
  sequence: number;
  version: number;
  status: MessageStatus;
  created_at: string;
  delivered_at?: string | null;
  read_at?: string | null;
  deleted_at?: string | null;
  failure_reason?: string | null;
  bundle?: { id: string; role: "member" | "display" };
}
export interface Conversation {
  id: string;
  org_id: string;
  participants: Actor[];
  last_message: Message | null;
  created_at: string;
  updated_at: string;
}
export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}
export type MessagePage = Page<Message>;
export type ConversationPage = Page<Conversation>;
export interface DraftInput extends Omit<MessageCreate, "text" | "sender_id"> {
  message_content?: string | null;
}
export interface Draft extends DraftInput {
  conversation_id: string;
  actor_id: string;
  version: number;
  updated_at: string;
}
export interface BundleCreate {
  message_ids: string[];
  display_message: MessageCreate;
}
export interface Bundle {
  id: string;
  conversation_id: string;
  original_message_ids: string[];
  display_message: Message;
  created_by: Actor;
  created_at: string;
  original_messages?: Message[];
}
export type BundleDetail = Bundle & { original_messages: Message[] };
export interface Presence {
  actor_id: string;
  availability: "online" | "offline";
  activity: Activity | null;
  last_seen_at: string | null;
}
export interface TestingEnvironment {
  environment_id: string;
  name: string;
  description?: string | null;
  organization_id: string;
  iam_environment_id: string;
  iam_app_id: string;
  status: string;
  version: number;
  creator_actor_id?: string;
  creator_actor_kind?: string;
  created_at: string;
  last_activity_at?: string;
  deleted_at?: string | null;
  purge_after?: string | null;
}
export interface CreateTestingEnvironment {
  name: string;
  description?: string;
  iam_environment_id: string;
  iam_environment_key: string;
  iam_app_id: string;
  iam_app_secret: string;
  iam_webhook_secret?: string;
  iam_webhook_key_version?: number;
}
export interface Profile {
  profile_id: string;
  actor: Actor;
  organization_id: string;
  testing_environment_id?: string | null;
  testing_generation?: number | null;
}
export interface Session {
  authenticated: boolean;
  actor?: Actor;
  organization_id?: string;
  profile_id?: string;
  testing_environment_id?: string | null;
  testing_generation?: number | null;
  profiles?: Profile[];
}
export type AuthenticatedSession = Session & Profile & { authenticated: true };
export interface AppConfig {
  iam_login_url: string;
  app_id: string;
  api_origin?: string;
  gateway_origin?: string;
  frontend_origin?: string;
  max_body_bytes?: number;
}
export interface LoginInput {
  slt: string;
  testing_key?: string;
  testing_environment_id?: string;
}
export interface MessageDelivery {
  type: "message";
  delivery_id: string;
  actor_id: string;
  delivery_sequence: number;
  message: Message;
}
export interface ReceiptDelivery {
  type: "receipt";
  delivery_id: string;
  actor_id: string;
  delivery_sequence: number;
  message_id: string;
  status: MessageStatus;
}
export type DurableDelivery = MessageDelivery | ReceiptDelivery;
export type ServerFrame =
  | DurableDelivery
  | {
      type: "ready";
      protocol_version: number;
      testing_generation: number | null;
      connection_id: string;
      actors: string[];
      acknowledged_through: Record<string, number>;
    }
  | { type: "ping"; ping_id: string }
  | { type: "message_accepted"; idempotency_key: string; message: Message }
  | { type: "receipt_recorded"; message_id: string; status: ReceiptStatus }
  | { type: "error"; code: string; message: string; recoverable: boolean };
export type RealtimeState =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "offline"
  | "closed"
  | "unauthorized"
  | "error";
export interface OutboxEntry {
  id: string;
  scope: string;
  profile_id: string;
  conversation_id: string;
  idempotency_key: string;
  generation: number | null;
  status: "queued" | "sending" | "failed" | "fenced";
  preview: string;
  created_at: number;
  updated_at: number;
  error?: string;
  error_code?: string;
}
