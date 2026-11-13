# OneGet Browser Capture Protocol

This document specifies the loopback protocol between the OneGet desktop application and its Chromium and Firefox extensions. Protocol version 1 covers health discovery, pairing, capture, and revocation. The desktop service is optional: a bind or pairing-database failure leaves ordinary downloads and settings usable, and the pairing surface reports the service as unavailable.

## Service and health

The service binds only to `127.0.0.1`. The production port is `37652`; a test build may bind an ephemeral port. The exact loopback authority is `127.0.0.1:<port>` and never `localhost`.

`GET /health` is unauthenticated. The response is:

```json
{
  "ok": true,
  "appVersion": "0.1.0",
  "protocolVersion": 1,
  "capabilities": ["capture", "prefill", "takeover"]
}
```

`GET /health` requires the exact `Host` header. It permits an absent `Origin` for a non-browser liveness check but rejects any present `Origin` that is not an exact browser-extension origin in a supported scheme. It returns no token, paired-client data, URL, filename, or filesystem path.

## Admission order

Every route is rejected before any task or prefill is created. The checks run in this order:

1. The request-header budget: at most 64 headers and at most 16 KiB across all header names and values.
2. The exact loopback `Host` authority.
3. The route-specific origin rule. `GET /health` allows an absent `Origin` and rejects a present non-extension origin. `POST /pair`, `POST /capture`, and `POST /pair/revoke` require an exact browser-extension origin.
4. A declared `application/json` content type for `POST /pair` and `POST /capture`. A `charset` or other parameter after the media type is allowed. `POST /pair/revoke` needs no body and does not require this header.
5. Bearer authentication for `POST /capture` and `POST /pair/revoke`. The token is read only from `Authorization: Bearer <token>`; a missing, malformed, query-string, or invalid token is rejected.
6. The per-route rate limit, then body reading and parsing, protocol version, and per-item validation.

A supported browser-extension origin is exactly one of two forms. A Chromium origin is `chrome-extension://` followed by 32 lowercase `a`–`p` characters. A Firefox origin is `moz-extension://` followed by a lowercase UUID in canonical 8-4-4-4-12 form, using only digits and `a`–`f` with hyphens at those fixed positions. An uppercase UUID, a UUID with a path, query, or fragment, another extension scheme, a web-page origin, `Origin: null`, a missing write origin, and a stolen token presented with a different origin are rejected. The accepted scheme changes no admission order, limit, token binding, or error code. No wildcard CORS header is returned.

## Limits

| Limit | Value |
| --- | --- |
| Request body | 65,536 bytes (64 KiB) |
| Capture items per request | 50 |
| URL length | 8,192 bytes (8 KiB) |
| Filename length | 255 UTF-8 bytes |
| Capture headers per item | 10 |
| Capture header name | 128 bytes |
| Capture header value | 2,048 bytes |
| Request headers | 64 count; 16 KiB combined name and value bytes |
| Pairing attempts per origin | 10 requests per 60 seconds |
| Capture requests per token | 120 requests per 60 seconds |
| Retained rate-limit keys | 256 per limiter |
| Retained duplicate records | 256 token-and-source entries |

A body over the limit returns 413. An oversized header set, too many items, an oversized URL, header, or filename, and an invalid body return 400. A protocol version other than 1 returns 409. A takeover request with anything other than exactly one item returns 400. A rate-limited request returns 429 with `Retry-After` in whole seconds and creates nothing. Limiter and duplicate state are bounded and evict the oldest entries.

## Pairing

The desktop generates an eight-digit decimal code with a ten-minute lifetime. Only one code is active: generating a new code invalidates an earlier one. The code is shown once on the desktop and is stored only as a SHA-256 hash.

`POST /pair` accepts:

```json
{
  "protocolVersion": 1,
  "code": "12345678",
  "label": "Optional client label"
}
```

`label` is optional, stripped of control characters, trimmed to 64 characters, and replaced with the default label when empty. A successful response is:

```json
{
  "token": "<64 lowercase hexadecimal characters>",
  "client": {
    "id": "<client id>",
    "origin": "chrome-extension://<32-character id>",
    "label": "Chrome extension",
    "createdAtMs": 1763000000000,
    "lastUsedAtMs": 1763000000000
  }
}
```

The token is 256 random bits rendered as 64 lowercase hexadecimal characters. It is returned exactly once. The desktop stores only its SHA-256 hash, bound to the exact origin that paired. A Firefox client returns its `moz-extension://<lowercase 8-4-4-4-12 UUID>` origin in the same field, and its label defaults to `Firefox extension` when none is supplied. The code is deleted in the same transaction that inserts the client, so a second use of the same code fails. A pending code is held in memory and is discarded when the app reopens, so a code shown before a restart cannot pair.

A wrong or expired code returns 401 with `INVALID_CODE`. Five failed attempts lock pairing for five minutes; while locked, requests return 429 with `PAIRING_LOCKED` and a `Retry-After` value. A successful pairing resets the failure count. Authorization compares the token hash and requires the exact bound origin; the last-used timestamp is persisted at most once per minute.

`POST /pair/revoke` requires the bearer token and exact origin and returns `{ "revoked": true }`. The desktop revoke action removes the client by identifier. After revocation, later captures with that token return 401, and the extension clears its stored token and asks the user to pair again. Deleting `pairing.db` revokes every client without affecting tasks, completed files, or settings.

The pairing database is a separate SQLite file, `pairing.db`, with schema version 1, WAL journaling, `synchronous=NORMAL`, a five-second busy timeout, and `0600` permissions on Unix. It contains a `clients` table, a single `pending_code` row, and a single `pairing_state` row. A newer schema returns 503 `PAIRING_SCHEMA_UNSUPPORTED`; an unreadable database returns 503 `PAIRING_DATABASE_UNAVAILABLE`.

## Capture request

`POST /capture` requires the bearer token and exact origin. The body is:

```json
{
  "protocolVersion": 1,
  "intent": "takeover",
  "sourceId": "takeover:123",
  "items": [
    {
      "url": "https://example.test/file.zip",
      "filename": "file.zip",
      "referer": "https://example.test/page",
      "mime": "application/zip",
      "size": 1048576,
      "kind": "file",
      "headers": [{ "name": "Referer", "value": "https://example.test/page" }]
    }
  ]
}
```

`intent` is `takeover` or `prefill`. `sourceId` is optional and is used for duplicate suppression. `items` must contain 1 to 50 entries. Each item accepts:

| Field | Rule |
| --- | --- |
| `url` | Required absolute `http` or `https` URL, at most 8 KiB, no embedded credentials, no control characters. |
| `filename` | Optional, non-empty other than `.` or `..`, at most 255 UTF-8 bytes, no `/`, `\`, or control characters. |
| `referer` | Optional absolute `http` or `https` URL without embedded credentials or control characters. |
| `mime` | Optional response MIME type. |
| `size` | Optional non-negative byte count. |
| `kind` | Optional `file`, `hls`, `dash`, or `video-platform`; absence means `file`. |
| `headers` | Optional safe headers. |

Only `Referer`, `User-Agent`, `Accept`, and `Accept-Language` are accepted as capture headers. Names are matched case-insensitively; a different name, a CR or LF in a name or value, or a duplicate normalized name is rejected. Cookies, authorization values, and arbitrary request headers are never accepted. Only `kind: "file"` is usable; `hls`, `dash`, and `video-platform` are rejected per item with `UNSUPPORTED_KIND`, and the UI explains that video parsing is not part of this capability.

A per-item failure does not discard the other items. A request with no usable item returns 400 `INVALID_CAPTURE_ITEM` and creates nothing.

## Capture response and semantics

A successful takeover returns:

```json
{
  "intent": "takeover",
  "accepted": 1,
  "rejected": [],
  "taskId": "<task id>",
  "duplicate": false
}
```

A repeated takeover with the same token and non-empty `sourceId` returns the existing task with `duplicate: true` and creates no second task. The extension must not cancel or erase the same browser download twice.

A successful prefill returns:

```json
{
  "intent": "prefill",
  "accepted": 1,
  "rejected": [{ "index": 0, "code": "UNSUPPORTED_KIND" }],
  "duplicate": false
}
```

Prefill replaces any earlier pending prefill with one bounded in-memory list, emits `app://capture-prefill`, and focuses the main window. It never creates a task. The desktop fetches the single pending prefill when the interface opens and also subscribes to the event, so an event that fires before subscription is still delivered. One item opens the New Download dialog with URL, filename, and a `Referer` header prefilled; multiple items open the Batch Download dialog with their URLs. If any dialog is already open, the prefill waits until it closes. Submitting the dialog creates one task per valid submitted item; dismissing it drops the prefill. Prefill content never survives an app restart. A new prefill replaces the previous pending one.

## Takeover sequence

The extension evaluates a browser download when the browser reports it. Chromium uses `downloads.onDeterminingFilename`; Firefox uses `downloads.onCreated`, and its `DownloadItem` is reduced to a safe basename or a URL-derived filename, with `totalBytes: -1` treated as an unknown size:

1. Read the stored token and the current settings. An unpaired extension allows the download.
2. Classify the target by filename extension first and MIME type second.
3. Refuse takeover when takeover is disabled, the URL is not HTTP or HTTPS, the download is incognito or extension-originated, the host is blacklisted exactly or as a subdomain, the target is an HLS or DASH manifest, the kind is unsupported, the category is disabled, or a known size is below the minimum. An unknown size is exempt from the minimum-size rule.
4. Send one takeover capture with a token-bound source identifier.
5. On success with a task identifier, cancel and then erase the browser download. On a duplicate, do not cancel again.
6. On 401, clear the token and report that pairing is no longer valid. On 409, report that OneGet or the extension must be updated. On any other failure, leave the browser download intact and show a localized, actionable error with the popup prefill path.
7. Chromium calls `suggest()` after the decision so a non-taken-over download proceeds normally. Firefox has no `onDeterminingFilename` and needs no equivalent call, so a fast download can start or finish before cancellation; the capture-first sequence limits the loss but does not remove that race.

The background duplicate set is in memory; a background restart does not create a second desktop task because the desktop deduplicates by the token-bound `sourceId`.

## Takeover failure surface

The background runtime keeps the most recent failed takeover and returns it as `takeoverError` from `popup/get-state`:

```json
{
  "downloadId": 7,
  "url": "https://cdn.test/files/movie.mp4",
  "filename": "movie.mp4",
  "reason": "capture_failed"
}
```

`reason` is one of `capture_failed` (a request failure or a response without a task identifier), `token_revoked` (401, after which the stored token is cleared), or `update_required` (409). The popup renders a localized banner with the filename-specific explanation and a Send to OneGet retry action that resends the same URL as a prefill, so the user can complete the download through the desktop dialog. The browser download remains in place throughout. Chromium keeps the record in memory. Firefox stores the same bounded record in browser session storage so an event-page suspension does not lose the popup retry path; it is never written to persistent storage, and both packages clear it after a later successful takeover or a successful prefill retry of the same URL.

## Error codes

Errors use a JSON body `{ "code": "<stable code>", "message": "<safe prose>" }`. Messages never carry a submitted code, token, header value, or request body.

| Status | Code | Meaning |
| --- | --- | --- |
| 400 | `HEADER_LIMIT` | Request or capture header count or size exceeded. |
| 400 | `INVALID_BODY` | The body is not valid JSON for the route. |
| 400 | `EMPTY_CAPTURE` | The capture contains no items. |
| 400 | `TOO_MANY_ITEMS` | More than 50 capture items. |
| 400 | `TAKEOVER_ITEM_COUNT` | A takeover request does not contain exactly one item. |
| 400 | `INVALID_CAPTURE_ITEM` | No item survived validation. |
| 401 | `UNAUTHORIZED` | Missing, malformed, unknown, revoked, or origin-mismatched token. |
| 401 | `INVALID_CODE` | Wrong, expired, reused, or superseded pairing code. |
| 403 | `INVALID_HOST` | Missing or non-exact loopback `Host`. |
| 403 | `INVALID_ORIGIN` | Missing or unsupported extension origin on a write route, or a non-extension health origin. |
| 409 | `PROTOCOL_MISMATCH` | Unsupported protocol version. |
| 413 | `PAYLOAD_TOO_LARGE` | Body above 64 KiB. |
| 415 | `UNSUPPORTED_MEDIA_TYPE` | Write body without `application/json`. |
| 422 | `TASK_CREATE_FAILED` | The engine could not create the takeover task. |
| 429 | `RATE_LIMITED` | The token exceeded its capture budget. |
| 429 | `PAIRING_LOCKED` | Five failed code attempts locked pairing. |
| 503 | `PAIRING_DATABASE_UNAVAILABLE` | The pairing database could not be opened. |
| 503 | `PAIRING_DATABASE_ERROR` | A pairing database operation failed. |
| 503 | `PAIRING_SCHEMA_UNSUPPORTED` | The pairing database schema is newer than this build. |
| 503 | `ENGINE_UNAVAILABLE` | The engine service is unavailable for a takeover. |
| 503 | `API_BIND_FAILED` | The loopback port could not be bound at startup. |

Per-item rejection codes are `INVALID_URL`, `CREDENTIALED_URL`, `UNSAFE_FILENAME`, `INVALID_REFERER`, `HEADER_LIMIT`, `FORBIDDEN_HEADER`, `DUPLICATE_HEADER`, and `UNSUPPORTED_KIND`. Rate-limited and locked responses carry `Retry-After` in whole seconds.

## Extension message flow

The popup and content scripts exchange typed messages with the extension background:

| Message | Direction | Purpose |
| --- | --- | --- |
| `resources:list` | Popup to worker | List the active tab's deduplicated resources. |
| `resources:clear` | Popup to worker | Clear a tab or frame's resources. |
| `settings:get` | Popup to worker | Read validated settings. |
| `settings:save` | Popup to worker | Validate, preserve unknown fields, and save settings. |
| `pairing:status` | Popup to worker | Report whether a token is stored. |
| `pairing:pair` | Popup to worker | Exchange a code for a token. |
| `pairing:unpair` | Popup to worker | Request revocation, then clear the local token. |
| `capture:prefill` | Popup to worker | Send one resource as a prefill capture. |
| `content/resource` | Content to worker | Report an observed resource URL, MIME type, and size. |
| `content/spa` | Content to worker | Reset the tab after a top-frame SPA route change, or only the sending frame after a subframe one. |
| `content/clear-frame` | Content to worker | Clear one frame's resources. |

Both packages listen to `webRequest.onResponseStarted`, `webNavigation.onCommitted`, and `tabs.onRemoved`. Chromium additionally listens to `downloads.onDeterminingFilename`; Firefox listens to `downloads.onCreated` and has no `onDeterminingFilename`. The background never reads a response body. The MAIN-world script wraps `fetch`, `XMLHttpRequest`, `history.pushState`, `history.replaceState`, `popstate`, and `hashchange`; it posts `{ source: "oneget-inject", kind, payload }` to the isolated content script, which forwards only HTTP(S) resource metadata.

## Permissions, CSP, and storage

The Chromium MV3 manifest requires Chrome 114 or later. The Firefox MV3 manifest uses `background.scripts` instead of a service worker, declares the Gecko ID `oneget@oneget.app`, requires Firefox 140 or later, and declares `data_collection_permissions.required: ["none"]`. Both request `webRequest`, `downloads`, `storage`, `tabs`, and `webNavigation`, plus host permissions for `http://*/*`, `https://*/*`, and the loopback API. Neither requests cookies, native messaging, a remote script source, or telemetry. The extension-page CSP is `script-src 'self'; object-src 'self';`, so no remote code and no eval are permitted. The build copies the local OneGet icons into each package.

The pairing token and settings use the browser's local storage area. Resource metadata uses the session storage area only and is bounded to 5,000 serialized entries. The background reloads the session snapshot after a service-worker or event-page restart and re-hydrates its per-tab store; Firefox also restores its bounded takeover-failure record from session storage. Page content, response bodies, cookies, and authorization values are never stored. Displayed URLs remove username, password, query, and fragment; the popup never renders the token.

## Manual verification expectations

Automated checks use a local fixture page and a mock loopback API. A real acceptance record must still identify the operating system, app build, extension build, Chrome version, Edge version, and Firefox version, and must record:

- loading the unpacked extension and opening the popup in Chrome, Edge, and Firefox;
- pairing from the popup with a desktop-generated code and confirming one paired client;
- sniffing responses, fetch, XHR, and media elements, with deduplication and correct tab and frame ownership;
- listing an HLS manifest and a DASH manifest while filtering their transport and media segments;
- sending one prefill and confirming the desktop dialog opens without creating a task until submit;
- a takeover that creates exactly one desktop task and removes the browser download only after success;
- an unpaired, revoked, or locked state that leaves the browser download untouched and reports actionable guidance;
- a Firefox event-page suspension after a failed takeover that still shows the popup retry path;
- Windows install and launch.

Any item not performed must be marked pending. A mock-service or headless result must not be recorded as manual browser evidence, and a pending item must not be recorded as passed.
