/** A paired browser client as exposed to the desktop settings surface. */
export interface PairedClientView {
  id: string;
  label: string;
  origin: string;
  createdAtMs: number;
  lastUsedAtMs: number;
}

/** A one-time pairing code and the instant it stops being valid. */
export interface PairingCodeView {
  code: string;
  expiresAtMs: number;
}

/** Browser-integration availability plus the current code and clients. */
export interface PairingStatus {
  available: boolean;
  reason: string | null;
  protocolVersion: number;
  code: PairingCodeView | null;
  clients: PairedClientView[];
}

export interface PrefillHeader {
  name: string;
  value: string;
}

/** One captured resource waiting to prefill a download dialog. */
export interface PendingPrefillItem {
  url: string;
  filename: string;
  referer: string;
  mime: string;
  size: number;
  headers: PrefillHeader[];
}

/** The single pending prefill batch held by the desktop process. */
export interface PendingPrefill {
  items: PendingPrefillItem[];
  receivedAtMs: number;
}
