# Automation & Webhooks

> [API.md](API.md#ops--observability) · [architecture.md](architecture.md)

SPACE includes an in-process automation engine and outbound webhook system for reacting to platform events.

## Automation Rules

Automation rules follow a **WHEN / IF / THEN** pattern:

```
WHEN event fires
  IF payload matches filter
    THEN execute actions
```

### Creating a Rule

```bash
curl -X POST /api/admin/automations \
  -H "Authorization: Bearer $ADMIN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Notify on new movies",
    "when_event": "media.added",
    "filter": { "media_type": "movie" },
    "actions": [
      { "type": "notify_all", "title": "New Movie", "body": "A new movie was added" }
    ]
  }'
```

### Supported Events

| Event | Fires When | Payload Fields |
|---|---|---|
| `media.added` | New media imported into catalog | `media_id`, `title`, `media_type` |
| `playback.started` | User begins playback | `user_id`, `media_id`, `source` |
| `payment.approved` | Card purchase approved | `user_id`, `card_count`, `merchant_id` |

### Filter Syntax

Filters are JSON objects with equality matchers. Every key in the filter must appear in the event payload with an equal value:

```json
{ "media_type": "movie" }
{ "media_type": "movie", "source": "ia-films" }
```

An empty object `{}` matches all events (no filtering).

### Action Types

| Type | Parameters | Description |
|---|---|---|
| `notify_all` | `title`, `body` | Push notification to all active users |
| `notify_users` | `user_ids[]`, `title`, `body` | Push notification to specific users |
| `webhook` | `event`, `payload` | Trigger an outbound webhook |

Example with multiple actions:

```json
{
  "name": "Movie added → notify + webhook",
  "when_event": "media.added",
  "filter": { "media_type": "movie" },
  "actions": [
    { "type": "notify_all", "title": "New Movie!", "body": "Check out the new addition" },
    { "type": "webhook", "event": "automation.custom.movie_alert", "payload": {} }
  ]
}
```

### Managing Rules

```bash
# List all rules
GET /api/admin/automations

# Toggle a rule on/off
PATCH /api/admin/automations/{id}
{ "active": true }

# Delete a rule
DELETE /api/admin/automations/{id}
```

### Execution Model

- Rules are evaluated in-process (no external job queue)
- `dispatch()` is called from the same trigger points that fire webhooks
- **Failures never propagate** — if a rule errors, it's logged and skipped
- Each execution increments the rule's `run_count` and emits `automation_runs_total`
- Rules are ordered by `created_at` and evaluated sequentially

## Outbound Webhooks

Webhooks deliver HMAC-SHA256 signed JSON payloads to external endpoints.

### Creating a Webhook

```bash
curl -X POST /api/admin/webhooks \
  -H "Authorization: Bearer $ADMIN" \
  -H "Content-Type: application/json" \
  -d '{ "url": "https://hooks.example.com/space", "events": "*" }'
```

**Response:**
```json
{
  "id": "...",
  "secret": "a1b2c3...",
  "note": "shown once; sign header is X-Signature (hmac-sha256 of raw body)"
}
```

> **The secret is shown only once at creation.** Store it securely.

### URL Requirements

- Must be `https://` (production)
- `http://localhost` and `http://127.0.0.1` allowed for testing

### Event Filtering

| `events` Value | Meaning |
|---|---|
| `*` | Receive all events |
| `media.added` | Only media import events |
| `playback.started` | Only playback events |
| `payment.approved` | Only payment events |

### Payload Format

Every webhook receives:

```json
{
  "event": "media.added",
  "ts": "2025-01-15T12:00:00Z",
  "data": { ... }
}
```

### Signature Verification

Each request includes:

| Header | Value |
|---|---|
| `X-Signature` | HMAC-SHA256 hex digest of the raw JSON body, keyed with the webhook secret |
| `X-Event` | The event name |
| `Content-Type` | `application/json` |

To verify:

```python
import hmac, hashlib

def verify_webhook(body: bytes, signature: str, secret: str) -> bool:
    expected = hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, signature)
```

### Delivery Behavior

- **Timeout:** 8 seconds per attempt
- **Retries:** 3 attempts with linear backoff (800ms, 1600ms, 2400ms)
- **Fire-and-forget:** Delivery runs in a spawned Tokio task; caller doesn't wait
- **Status tracking:** `last_status` (HTTP code or `network_error`) and `last_fired_at` are stored on the webhook record

### Metrics

```
webhook_deliveries_total{event="media.added", status="200"} 12
webhook_deliveries_total{event="media.added", status="500"}  2
```

### Managing Webhooks

```bash
# List (secrets masked)
GET /api/admin/webhooks

# Test-fire (sends { "test": true } with event "*")
POST /api/admin/webhooks/{id}/test

# Delete
DELETE /api/admin/webhooks/{id}
```

### Automation → Webhook Bridge

The `webhook` action type in automation rules bridges the two systems. When an automation rule fires a `webhook` action, it calls `webhooks::emit()` with the specified event name, which then delivers to all matching webhook subscriptions.

This means you can create compound workflows:

1. `media.added` fires automation rule
2. Rule matches filter `{ "media_type": "movie" }`
3. Rule's `webhook` action fires with event `automation.custom.new_movie`
4. External webhook endpoint receives the signed delivery

## Observability

Both systems emit Prometheus metrics:

| Metric | Labels | Description |
|---|---|---|
| `automation_runs_total` | `event` | Rules executed per event type |
| `webhook_deliveries_total` | `event`, `status` | Delivery attempts and results |

View recent activity:

```bash
GET /api/admin/events?event=automation.executed&limit=20
```
