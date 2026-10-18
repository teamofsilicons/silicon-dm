/**
 * The DM service's wire shapes as the website reads them (openapi.yaml, docs/reference/wire-format.md). Every response
 * is an envelope `{"type": …, "data": …}`; these are the `data` parts. Fields the service is adding in 0.17 (spec §4:
 * `speech`, `voice`, `gif`, `attachment_details`, `metadata`, `unread_count`, `last_read_message_id`, `speech` on
 * /me) are optional, so the website works against an older service and a newer one alike.
 */

export type AccountKind = "carbon" | "silicon";

/** An account as the service names it: always the current id, and the permanent uuid to key on. */
export interface AccountRef {
  type: AccountKind;
  /** The current id, `c:handle` or `si:handle` (the uuid for a deleted account). */
  id: string;
  uuid?: string;
  display_name?: string;
  pfp_url?: string;
  deleted?: boolean;
  legacy?: boolean;
  /**
   * A Silicon's custodian (the Carbon who looks after it), one level deep. A newer service sends it wherever it names
   * a Silicon; absent on Carbons, from an older service, and when DM knows no custodian. When DM has not seen the
   * custodian's id yet, its `id` is its uuid (read it through `custodianFromRef`, lib/chat/address.ts).
   */
  custodian?: AccountRef | null;
}

export interface Delivery {
  transport: "sync" | "ting";
  ting_enabled: boolean;
  ting_api_base_url?: string | null;
  enrolled?: boolean;
  enrolment?: string;
  sync_path: string;
  sync_long_poll_seconds: number;
  presence_path?: string;
  receipts_path?: string;
}

/** Whether this DM speaks and transcribes (spec §4.7); absent on an older service. */
export interface SpeechSupport {
  enabled: boolean;
  model?: string;
  stt_model?: string;
  voices_path?: string;
  /** The service's speech limits (DM 0.17; absent on an older service): what one script, a minute and a day may take. */
  limits?: {
    script_characters: number;
    renders_per_minute: number;
    daily_characters: number;
    transcribe_max_bytes: number;
    transcriptions_per_minute: number;
  };
}

export interface Me {
  account: AccountRef;
  custodian: AccountRef | null;
  /** The Silicons this Carbon looks after. */
  custodian_of: AccountRef[];
  scopes: string[];
  via_app: string | null;
  delivery: Delivery;
  speech?: SpeechSupport;
}

/** An attachment with what the sender said about it (spec §4.6), in the same order as `attachments`. */
export interface AttachmentDetail {
  url: string;
  name?: string;
  content_type?: string;
  size?: number;
}

/** A voice note: an audio link with its length (and, in `voice_transcript`, its words). */
export interface VoiceNote {
  url: string;
  content_type?: string;
  size?: number;
  duration_milliseconds?: number | null;
}

export interface Gif {
  provider_id: string;
  url: string;
  preview_url?: string;
  title?: string;
}

/** A Silicon's spoken message (spec §4.3): the script, the voice pinned on the message, and its render state. */
export interface Speech {
  voice: string;
  voice_name?: string;
  script: string;
  model?: string;
  status: "pending" | "ready" | "failed";
  duration_milliseconds?: number | null;
  /** 64 bar heights, 0 to 100, for the waveform. */
  peaks?: number[] | null;
}

export interface QuotedContent {
  message: string | null;
  attachments: string[];
  voice_transcript: string | null;
}

export interface Reply {
  "message-id": string;
  sender: AccountRef;
  /** The quoted message as it is now; null once it was deleted. */
  content: QuotedContent | null;
}

export interface BundleRef {
  id: string;
  role: "display" | "member";
}

export interface HistoryEntry extends QuotedContent {
  reply: Reply | null;
  /** When this content became current. */
  created_at: string;
}

export interface Message {
  /** The conversation-local code, `000` … `zzz`, then `1000`. */
  "message-id": string;
  conversation_id: string;
  /** The other account, the group, or a routing address such as `deliberate@si:cos` (ISI). */
  recipient_id: string;
  sender: AccountRef;
  /** Empty for attachment-only messages; null for a tombstone. */
  message: string | null;
  attachments: string[];
  voice_transcript: string | null;
  reply: Reply | null;
  bundle: BundleRef | null;
  history: HistoryEntry[];
  created_at: string;
  updated_at: string | null;
  deleted_at: string | null;
  /** When every recipient had it on a device. */
  delivered_at: string | null;
  /** When every recipient read it. */
  read_at: string | null;
  /** The app that sent it for its sender (a User verification proof), such as `interface`. */
  via_app?: string;
  attachment_details?: AttachmentDetail[];
  voice?: VoiceNote | null;
  gif?: Gif | null;
  speech?: Speech | null;
  metadata?: Record<string, unknown>;
  /**
   * The routing address its sender gave (an ISI such as `release-notes@si:sol`), with current ids, or null when it was
   * not routed (DM 0.17); absent on an older service, where `recipient_id` carries it in a direct conversation only.
   */
  routed_to?: string | null;
}

export interface GroupSummary {
  name: string;
  description: string;
  owners: AccountRef[];
}

export interface GroupDetails extends GroupSummary {
  version: number;
  /** Accepted members, owners included. */
  members: AccountRef[];
  /** Only owners (and custodians acting for them) see these. */
  pending_invitations: AccountRef[];
}

export type LastMessageStatus = "sent" | "delivered" | "read" | null;

/** One row of GET /api/v1/conversations (and, with `group` details, a single conversation). */
export interface ConversationSummary {
  id: string;
  participants: AccountRef[];
  visible_to_custodians?: AccountRef[];
  supersedes?: string[];
  superseded_by?: string;
  group?: GroupSummary | GroupDetails;
  last_message: Message | null;
  last_message_status?: LastMessageStatus;
  created_at: string;
  updated_at: string;
  /** Messages from others not yet read by the listed account (spec §4.5); absent on an older service. */
  unread_count?: number;
  /** The last message the listed account read, or null (spec §4.5). */
  last_read_message_id?: string | null;
  /** The listed account's draft version here (what GET …/draft answers), or null when it has none; absent on an older service. */
  draft_version?: number | null;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

/**
 * A change to a conversation as it concerns you (DM 0.17, only with `include=conversations`; GET /api/v1/contracts
 * announces it as `features.conversation_events`): it was created with you, you were invited, added, or took part in
 * an invitation's answer, you left or were removed, the group was renamed, described or changed its members or
 * owners, or you read it (on any device: `read`, with its unread count and read position as of the event's place in
 * the feed). A value a later service adds is treated as "something changed": the conversation is asked for again.
 */
export type ConversationChange =
  | "created" | "invited" | "added" | "invitation_accepted" | "invitation_declined" | "invitation_cancelled"
  | "left" | "removed" | "renamed" | "description_changed" | "members_changed" | "owners_changed" | "read";

export type SyncEvent =
  | { event_id: string; sequence: number; type: "message" | "message_status"; conversation_id: string; message_id: string }
  /** No message_id: the event is about the conversation (its `id` as the list names it: g:…, c:a::si:b, or a UUID). */
  | {
      event_id: string;
      sequence: number;
      type: "conversation";
      conversation_id: string;
      change: ConversationChange | (string & {});
      message_id?: undefined;
      /** `read` only: your unread messages there, and the newest you read, as of this event (DM 0.17). */
      unread_count?: number;
      last_read_message_id?: string | null;
    };

export interface SyncPage {
  events: SyncEvent[];
  cursor: string;
  has_more: boolean;
  upper_sequence: number;
}

export type Activity = "typing" | "recording_voice" | "transcribing_voice" | "uploading_file" | "searching_gifs";

export interface Presence {
  member_id: string;
  member_uuid?: string;
  availability: "online" | "offline";
  activity: Activity | null;
  last_seen_at: string | null;
}

export interface PresenceLease {
  presence: Presence;
  lease_expires_at: string;
  activity_expires_at: string | null;
}

export interface DraftAttachment {
  permanent_url: string;
  name?: string;
  content_type?: string;
  size?: number;
}

export interface DraftVoice extends DraftAttachment {
  duration_milliseconds?: number | null;
}

export interface Draft {
  conversation_id: string;
  member_id: string;
  member_uuid?: string;
  version: number;
  message_content?: string;
  attachments?: DraftAttachment[];
  voice?: DraftVoice;
  voice_transcript?: string | null;
  gif?: Gif;
  metadata: Record<string, unknown>;
  updated_at: string;
  reply_to_message_id?: string | null;
}

export interface DraftInput {
  message_content?: string;
  attachments?: DraftAttachment[];
  voice?: DraftVoice;
  voice_transcript?: string | null;
  gif?: Gif;
  metadata?: Record<string, unknown>;
  reply_to_message_id?: string | null;
}

export interface Invitation {
  conversation_id: string;
  name: string;
  description: string;
  invitee: AccountRef;
  invited_by?: AccountRef;
  invited_at?: string | null;
  owners: AccountRef[];
}

export interface AllowListEntry {
  account: AccountRef;
  added_by?: AccountRef;
  added_at: string;
}

export interface AllowList {
  silicon: AccountRef;
  items: AllowListEntry[];
  note?: string;
}

/** What a send carries (POST …/messages, `message.create`). Attachments may be links or objects with details. */
export interface MessageCreate {
  message?: string;
  attachments?: Array<string | DraftAttachment>;
  voice?: DraftVoice;
  voice_transcript?: string | null;
  gif?: Gif;
  reply?: { "message-id": string } | null;
  metadata?: Record<string, unknown>;
  /** A Silicon's spoken message (spec §4.3). The website never speaks as a Carbon, but the type is the service's. */
  speech?: { script: string; voice?: string };
  recipient_id?: string;
}

export interface ReceiptsResult {
  updated: number;
  unread_count: number;
  last_read_message_id: string | null;
}

/** A voice of the catalog (spec §4.2). */
export interface VoiceOption {
  key: string;
  name: string;
  description: string;
  voice_id?: string;
}

export interface VoiceCatalog {
  model: string;
  items: VoiceOption[];
  default_rule: string;
}

export interface AccountVoice {
  account: AccountRef;
  voice: VoiceOption;
  chosen: boolean;
  chosen_by: AccountRef | null;
  updated_at: string | null;
  can_change: boolean;
}

export interface BundleDetail {
  id: string;
  conversation_id: string;
  original_message_ids: string[];
  display_message: Message;
  created_by: AccountRef;
  created_at: string;
  original_messages: Message[];
}
