// Mirrors the Swift models in teil.ing-client/Models. The Rust API client deserializes
// the snake_case teil.ing API into these camelCase shapes and hands them to the frontend,
// so field names here match the Rust `serde(rename_all = "camelCase")` structs.

/** "image" | "video" — the server says to branch on this, not on mimeType. */
export type MediaKind = "image" | "video";

/** Server-side video pipeline state; null for images. */
export type StreamStatus = "uploading" | "processing" | "ready" | "failed";

/** GET /api/v1/images/:id — full image metadata. (Swift: ImageResponse) */
export interface ImageResponse {
  id: string;
  slug: string;
  originalFilename: string;
  mimeType: string;
  fileSize: number;
  imageUrl: string | null;
  thumbnailUrl: string | null;
  hasPassword: boolean;
  isPrivate: boolean;
  viewCount: number;
  maxViews: number | null;
  validUntil: string | null;
  isEdited: boolean;
  createdAt: string;
  kind: MediaKind;
  streamStatus: StreamStatus | null;
  durationMs: number | null;
  width: number | null;
  height: number | null;
  /** Token-signed HLS playlist; expires — never cache. Null until streamStatus is "ready". */
  videoUrl: string | null;
}

/** GET /api/v1/quota. (Swift: QuotaResponse) */
export interface QuotaResponse {
  storageUsed: number;
  storageQuota: number | null;
  tier: string;
  imageCount: number;
}

/** 201 response from POST /api/v1/upload. (Swift: UploadResponse) */
export interface UploadResponse {
  id: string;
  slug: string;
  shareUrl: string;
  imageUrl: string | null;
  thumbnailUrl: string | null;
  isPrivate: boolean;
}

/** PATCH /api/v1/images/:id body. (Swift: ImageUpdateRequest) */
export interface ImageUpdateRequest {
  password?: string;
  removePassword?: boolean;
  private?: boolean;
  maxViews?: number;
  validForDays?: number;
}

/** What the clipboard receives after upload. (Swift: ClipboardMode) */
export type ClipboardMode = "url" | "image";

/** User preferences — mirrors Swift PreferencesStore, persisted Rust-side. */
export interface Prefs {
  stripExif: boolean;
  openInBrowser: boolean;
  clipboardCopy: boolean;
  launchAtLogin: boolean;
  autoCheckForUpdates: boolean;
  privateUpload: boolean;
  clipboardMode: ClipboardMode;
  /** Folder downloads are saved to; null = the OS Downloads folder. */
  downloadDir: string | null;
  /** Show a save dialog per download instead of writing straight to downloadDir. */
  askWhereToSave: boolean;
  /** Recording frame rate: 30 or 60. */
  videoFps: number;
  /** Include system audio in recordings. */
  videoCaptureAudio: boolean;
  /** Include the mouse pointer in recordings. */
  videoShowCursor: boolean;
}

/** A capturable on-screen window + geometry (points, top-left origin), from `list_windows`. */
export interface WindowInfo {
  id: number;
  title: string;
  appName: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Upload lifecycle events emitted from Rust (Swift: UploadFeedbackEvent). */
export type UploadFeedback =
  | { kind: "started" }
  | { kind: "succeeded"; imageId: string; shareUrl: string }
  | { kind: "failed"; message: string };

/** The three capture modes (Swift: region/fullscreen/window). */
export type CaptureMode = "region" | "fullscreen" | "window";

/** Recording targets mirror the capture modes. */
export type RecordMode = CaptureMode;

/** Recording lifecycle as reported by Rust (recording.rs). */
export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping" | "uploading";

/** Payload of the "recording-state" event and get_recording_status. */
export interface RecordingStatus {
  supported: boolean;
  phase: RecordingPhase;
  mode: RecordMode | null;
  elapsedMs: number;
  bytesWritten: number;
  uploadSent: number | null;
  uploadTotal: number | null;
}

/** Payload of the "upload-progress" event (streaming video upload). */
export interface UploadProgress {
  sent: number;
  total: number;
}

/** Selected region in global virtual-desktop coordinates. */
export interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
}
