# Nexus Gateway API — `/api/v1`

The surface Daedalus IT (the Hub) and the rmm-agent talk to
(`src/controllers/gateway.rs`). JSON is camelCase; where a stored value is
absent it is rendered as an empty string or zero rather than `null`, so the
Hub's Go structs always decode.

## Authentication

Every route takes `Authorization: Bearer <key>`: the root
`NEXUS_SYSTEM_TOKEN` or a token minted with `POST /api/nexus/tokens`. A missing
or unknown key is `401`.

## Endpoints

| Method | Path | Does |
|---|---|---|
| GET | `/agents` | every agent |
| GET | `/agents/{id}` | one agent with its facts |
| POST | `/agents/enroll` | `{hostname, hostgroup?}` → a new agent |
| POST | `/agents/{id}/report` | facts; counts as a heartbeat |
| POST | `/agents/{id}/heartbeat` | heartbeat |
| GET/POST | `/agents/{id}/environment` | `{environment, monitored, note}` |
| GET | `/agents/{id}/logs?limit=` | journal tail from the Logger (1–1000) |
| POST | `/agents/{id}/logs` | agent ships log lines |
| GET | `/agents/{id}/tasks` | the agent's planned/dispatched tasks (marks them `dispatched`) |
| POST | `/agents/{id}/tasks/{taskId}/result` | the agent reports a task's result — see below |
| POST | `/tasks` | `{intent, targets, requesterId?, autoRollback?, params?}` → `{taskId, status}` |
| GET | `/tasks/{id}` | one task's lifecycle and result — see below |

## Task lifecycle

`status` on a task is one of:

| Status | Meaning |
|---|---|
| `pending` | only for an instant while `POST /tasks` creates the row (and on tasks made through the operator API `/api/tasks`) |
| `accepted` | recorded, but the Orchestrator could not plan it; no agent will pick it up |
| `planned` | the Orchestrator returned a plan; waiting for the agent to poll |
| `dispatched` | an agent has been handed the plan |
| `completed` | the agent reported `success` |
| `failed` | the agent reported anything else |
| `cancelled` | cancelled through the operator API |

`completed`, `failed` and `cancelled` are terminal. A caller following a task
polls `GET /tasks/{id}` until `status` is one of them (an `accepted` task never
moves on by itself).

## `GET /api/v1/tasks/{id}`

```json
{
  "taskId": "6f1c…",
  "intent": "run_command",
  "status": "failed",
  "targets": ["0b7e…"],
  "createdAt": "2026-10-08T12:00:00+00:00",
  "updatedAt": "2026-10-08T12:00:41+00:00",
  "completedAt": "2026-10-08T12:00:41+00:00",
  "result": {
    "status": "failed",
    "exitCode": 3,
    "message": "executed run_command (1 steps)",
    "error": "exit status 3",
    "output": "==> command.run [failed]\n…\nerror: exit status 3",
    "steps": [
      {"id": "s1", "action": "command.run", "status": "failed", "changed": true, "output": "…", "error": "exit status 3"}
    ]
  }
}
```

- `targets` are agent ids. `completedAt` is `""` until the task is terminal.
- `result` is `null` until the agent reports. `result.status` is the agent's
  own verdict (`success` / `failed`); `exitCode` is what the agent sent, or `0`
  for `success` and `1` otherwise when it sent none. `steps` is `[]` when the
  agent sent none; step `status` is `success`, `failed` or `skipped` (from the
  rmm-agent).
- An id that names no task (or is not a UUID) is `404 {"error":"not_found"}`.

## `POST /api/v1/agents/{id}/tasks/{taskId}/result`

```json
{
  "status": "success | failed",
  "error": "…",
  "message": "…",
  "exitCode": 0,
  "output": "…",
  "steps": [{"id": "…", "action": "…", "status": "…", "changed": false, "output": "…", "error": "…"}]
}
```

Only `status` is required; `exitCode`, `output` and `steps` (and every field of
a step) are optional, so an agent that sends just `{status, error?, message?}`
still works. `steps[].id` is also accepted as `stepId`.

Stored sizes are capped; longer text keeps its **tail** (where a failing script
says why) behind a `[... truncated N bytes ...]` marker: `output` 64 KiB, each
step's `output` 16 KiB and `error` 4 KiB, at most 256 steps. When the body has
steps but no `output`, the stored output is built from the steps, each under a
`==> action [status]` header. A second report for the same task overwrites the
first. Responds `{taskId, status}`.

### Forward to the Hub

When `DAEDALUS_INGEST_URL` is set, every result is also pushed (best-effort) to
`$DAEDALUS_INGEST_URL/api/v1/automation/results/task` with
`X-Daedalus-Automation-Key: $DAEDALUS_INGEST_KEY`:

```json
{"linexusTaskId": "…", "status": "success | failed", "exitCode": 0, "output": "…"}
```

`exitCode` is the reported one (else 0/1 as above). `output` is the stored run
output (capped as above); when there is none, the agent's `error`, else its
`message`.
