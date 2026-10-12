# WebSocket Protocol

Single endpoint: `GET /ws?token=<access_token>` (session token; API keys are
not accepted on the socket). On success the socket is upgraded; on failure you
get an HTTP 401 before upgrade.

## Framing

Every server→client message is JSON:

```json
{ "type": "event", "room": "party:9f…", "data": { … } }
```

Client→server frames are also JSON, one per text message.

## Rooms

| Room | Purpose |
|---|---|
| `__global__` | presence changes + platform event envelopes (auto-subscribed) |
| `user:<your_uuid>` | your notifications, security flags, AI notices (auto-subscribed, self-only) |
| `conv:<conversation_id>` | direct/group/party chat messages, typing |
| `party:<party_id>` | playback sync, reactions, join/leave |

Joining a room requires authorization — the server validates every subscribe
and every event: conversation/party membership tables for those rooms, strict
self-match for `user:` rooms.

## Client → Server

```json
{ "type": "ping" }
{ "type": "subscribe",   "room": "conv:…" }
{ "type": "unsubscribe", "room": "conv:…" }
{ "type": "typing",      "room": "conv:…" }
{ "type": "message.send", "room": "conv:<uuid>", "body": "hi" }
{ "type": "read",         "room": "conv:<uuid>" }
{ "type": "reaction",     "room": "party:<uuid>", "emoji": "😱", "second_offset": 812 }
```

`message.send` persists to Postgres, fans out to all room subscribers as
`message.new`, and replies with an ack:

```json
{ "type": "message.sent", "data": { "id": "<message uuid>" } }
```

Errors come back inline:
```json
{ "type": "error", "data": { "message": "not allowed to join this room" } }
```

## Server → Client event types

| Event | Room | Payload (`data`) |
|---|---|---|
| `hello` (on connect) | global | `{ user_id, online_users[] }` |
| `presence` | global | `{ user_id, online, online_count }` |
| `platform.event` | global | `{ event, data }` — universal event bus envelope (see below) |
| `notification` | global + your `user:` room | Notification object |
| `security.flag` | your `user:` room | `{ decision: "challenge"\|"revoke", score }` |
| `typing` | conv/party | `{ user_id, display_name }` |
| `message.new` | conv | full ChatMessage object |
| `reaction` | party | `{ user_id, emoji, second_offset }` |
| `state.sync` | party | PartySyncState (see below) |
| `join.event` / `leave.event` | party | `{ user_id, display_name? }` |
| `party.ended` | party | `{ party_id }` |
| `pong` | — | `{ ts }` |

### Platform event envelopes

The event bus (`events` table) bridges onto the global room:

```json
{ "type": "event", "room": "__global__",
  "data": { "type": "platform.event",
            "event": "resolution.issued",
            "data": { "source": "…", "kind": "hls", "quality": "1080p" } } }
```

Current events: `resolution.issued`, `playback.authorized`, `progress.updated`.
New capabilities subscribe to these instead of coupling to producers; the
timeline is also queryable at `GET /api/admin/events`.

### PartySyncState

```json
{ "seq": 42, "playing": true,
  "position_seconds": 611.3,        // position at last host action
  "server_time_ms": 1767225600123,  // wall clock when this was sent
  "server_position_seconds": 623.9  // projected NOW (position + elapsed)
}
```
Clients should render from `server_position_seconds` and correct drift by
comparing their clock offset with `server_time_ms`.

## Minimal JavaScript client

```js
const ws = new WebSocket(`wss://host/ws?token=${token}`);
ws.onopen = () => ws.send(JSON.stringify({ type: "ping" }));
ws.onmessage = (e) => {
  const frame = JSON.parse(e.data);
  if (frame.type === "event") console.log(frame.room, frame.data.type, frame.data.data);
};
ws.send(JSON.stringify({ type: "subscribe", room: "conv:<uuid>" }));
ws.send(JSON.stringify({ type: "message.send", room: "conv:<uuid>", body: "hello" }));
```

Heartbeat (v2): the server sends a protocol-level ping every 30 s and reaps
connections that show no activity for 75 s — clients should answer pongs
(browsers do this automatically). Application-level `{"type":"ping"}` /
`{"type":"pong"}` still works and also counts as liveness. Disconnects are
clean — rooms die with the socket and presence decrements automatically.
