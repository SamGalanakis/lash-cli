# E2E Scenario: Process Dock Lifecycle — Visibility, Cancel, and the Deletion Invariant

> **Read [../RULES.md](../RULES.md) first** — operator surface, poll-don't-sleep, stop
> triggers, and reporting/RCA conventions. This runbook only adds the scenario-specific
> parts.

**Purpose.** Prove the CLI process dock's contract: a **Runtime Process** shows in the dock
(header `Background`), the focused process can be cancelled (`Delete`), and — the invariant
— **ending or deleting a session never ends a process by itself**. The dock uniformly
reflects the session's durable process registry (`session.processes().list()`). A subagent
spawn is a durable Runtime Process and therefore renders in two places: inline tool
activity (`◆ spawn subagent · …`, footer `Running tool · spawn_agent`) and a transient dock
row (`Background`, `◆ running · subagent · spawn`). Its durable record in
`store/processes.db` moves from `process.first_started` to `process.completed`, carries an
observer edge to the parent session, and the settled dock row is pruned after the short
retention window.

**Why this matters.** The Lash repository's CONTEXT.md → "Runtime Process" says its
lifecycle is independent of any session and only runtime processes appear in the CLI
process dock. That independence is the whole point of a durable process; if session
teardown silently killed it, the durability guarantee would be a lie.

## Pre-flight harness limits (read before scoring)

**A subagent Runtime Process is reachable under `--provider test`, but not long enough to
drive every lifecycle control.** At the pinned Lash revision, the deterministic
`rlm-subagent-smoke` scenario starts a durable process:

- There is **no `/process` slash command** — the builtin set is `/clear /compact /controls
  /fork /tree /version /info /model /variant /mode /provider /logout /retry /resume /skills
  /help /exit` (`crates/lash-cli/src/command.rs`). The dock is a render surface fed by
  `session.processes().list()`, not a command.
- The RLM `rlm-subagent-smoke` scenario spawns a subagent registered as
  `ProcessIdentity("subagent", label "spawn")`. It renders both inline tool activity and a
  `Background` dock row, and writes its lifecycle plus observer edge to
  `store/processes.db`.
- That deterministic subagent completes in under a second. The transient row is therefore
  not reliably focusable before completion, so `Tab`/`Delete` and the session-deletion
  invariant remain un-drivable with this scenario.
- The deterministic test provider only returns canned responses, so no scenario leaves a
  durable Runtime Process running long enough to focus, inspect, cancel, or test survival
  across session deletion. A **Process Engine** contributed by an installed plugin can
  provide such a long-running process for the positive procedure.

Per [../RULES.md](../RULES.md)'s "missing capability → note as harness gap": this runbook
runs the drivable subagent admission/lifecycle gates and the empty-dock gate under
`--provider test`, then specifies the remaining positive procedure for `--provider real`
with a process-engine plugin. Do not pretend the sub-second process offers a focus/cancel
window that it does not.

## Scenario-specific golden rules

1. **Subagent = inline activity + dock process.** A spawned subagent must render both the
   inline `spawn_agent` activity and the transient `Background` process row. Either surface
   missing is a contract failure.
2. **The deletion invariant is the crown jewel.** In the positive (real-provider) run, the
   process must **survive** its originating session being ended/deleted. A process that dies
   with its session is a hard fail.
3. **Prove admission durably.** The dock row must correspond to a `subagent / spawn` row in
   `store/processes.db`, the `process.first_started` and `process.completed` lifecycle
   events, and an observer edge to the parent session.
4. **Don't invent a control window.** The deterministic subagent is a real process, but its
   sub-second lifetime does not make `Tab`/`Delete` or session-deletion survival drivable.

## Phase 0 — Pre-flight

Per [../RULES.md](../RULES.md). For the negative-space gates, launch
`scripts/lash-operator.py --provider test --scenario rlm-subagent-smoke -- -em rlm` and
confirm the deterministic provider and idle prompt.

## Phase 1 — Drivable gates under `--provider test`

**Subagent renders as both tool activity and a durable dock process.** Use a fresh fixed
`--lash-home $LH` so its process store can be checked immediately after settlement.

```
type Does your subagent tool work
key enter
expect 15 Running tool · spawn_agent
expect 15 Background
expect 15 ◆ running · subagent · spawn
clear
expect 45 ■ subagent-ok
screen 40
```

Gate: the spawn shows as `◆ spawn subagent · …`, the footer passes through `Running tool ·
spawn_agent`, and the live process projection renders `Background` with `◆ running ·
subagent · spawn`. Keep the `clear` after the running-tool gates so `■ subagent-ok` must
come from a fresh settled frame. In that settled `screen`, the transient `Background` row
is absent after its retention window.

Before `lash-exit`, cross-check the fixed home's process store from a second shell:

```
sqlite3 -header -column "$LH/store/processes.db" "
SELECT process_id, identity_kind, identity_label, status
FROM processes
WHERE identity_kind = 'subagent' AND identity_label = 'spawn';
SELECT sequence, event_type
FROM process_events
WHERE process_id = (
  SELECT process_id FROM processes
  WHERE identity_kind = 'subagent' AND identity_label = 'spawn'
)
AND event_type IN ('process.first_started', 'process.completed')
ORDER BY sequence;
SELECT session_id, process_id
FROM process_observers
WHERE process_id = (
  SELECT process_id FROM processes
  WHERE identity_kind = 'subagent' AND identity_label = 'spawn'
);"
```

Gate: exactly one process row reports `subagent`, `spawn`, and `completed`; exactly two
selected lifecycle rows appear in order, `process.first_started` then `process.completed`;
and exactly one observer row links that process id to the parent session. Then
`lash-exit 10`.

**Empty dock has nothing to focus.** Relaunch `--scenario standard-echo`. With an idle empty
prompt and no processes, the dock-focus keys have no target:

```
expect 20 Message · / for commands
key tab
wait 1
screen 18
```

Gate: no `Background` dock, and `Tab` opens no process overview (the dock-focus binding —
`docs/index.html`: "With an empty prompt, cycle focus through the dock of background
processes" — falls through when the dock is empty; note that `Shift+Tab` here falls through
to the plan-mode toggle, further evidence there is no dock to cycle). The process **cancel**
(`Delete`) and **overview** (`Enter`) rungs cannot be exercised — no process exists to
focus. Record this as the harness gap. Then `lash-exit 10`.

## Phase 2 — Remaining positive procedure (`--provider real`, needs a process-engine plugin)

The deterministic subagent already proves process admission, dock visibility, completion,
and durable observation, but it is too short-lived for focus/cancel or the deletion
invariant. A real-provider run (RLM mode plus a plugin whose tool starts a **long-running
durable Runtime Process**, e.g. a background job that yields a process handle) can execute
the remaining controls verbatim. Launch
`scripts/lash-operator.py --provider real -- --model <provider/model> -em rlm` (spends
tokens — deliberate).

1. **Start a process.** Drive a turn whose tool starts a durable background process. Gate:
   the `Background` dock renders a row `◆ running · <producer> · <label> · <elapsed>`
   (`crates/lash-cli/src/render/sections/docks.rs`). Objective cross-check:
   `LASH_HOME/store/processes.db`
   records the process; a `list_process_handles` activity, if driven, reports it `running`.
2. **Focus and inspect.** With an empty prompt, `Tab` to focus the dock row (it gains the
   `SELECTED` badge / `▶` glyph); `Enter` opens its overview. Gate: the focused row and the
   overview name the same process.
3. **Cancel path.** `Delete` on the focused process. Gate: a `Process \`<label>\`
   cancellation requested: …` message, and the dock row transitions `running → cancelled`
   (or drops after its transient window). Objective: the process store shows a terminal
   `cancelled` state.
4. **Deletion invariant (crown jewel).** Start a **fresh** process, then delete/end its
   **session** (`/clear` opens a new session, retiring the current one; or delete the
   session db out of band). Gate: the process **remains** — still `running` in the process
   store and still listed by `list_process_handles` for a session it is granted to. A
   process that ends because its session ended → **hard fail** (contract violation) →
   Abort/RCA.

## Phase 3 — Score

| Item | Objective gate | Verdict | Notes |
|------|----------------|---------|-------|
| Subagent renders on both surfaces | `◆ spawn subagent` + `Running tool · spawn_agent` + `Background` / `◆ running · subagent · spawn`, then post-`clear` `■ subagent-ok` |  |  |
| Durable subagent lifecycle | one `subagent / spawn / completed` process row; `process.first_started → process.completed`; one parent-session observer edge |  |  |
| Transient row is pruned | settled post-`clear` screen has no `Background` row |  |  |
| Empty dock has no focus target | `Tab` opens no overview; no `Background` header |  |  |
| Dock process visible *(real only)* | `Background` row `◆ running · … `; process store row |  |  |
| Cancel path *(real only)* | `running → cancelled`; terminal state in store |  |  |
| Deletion invariant *(real only)* | process survives its session being ended |  |  |
| Harness gap recorded | sub-second process leaves focus/cancel/deletion rungs un-drivable under `--provider test` |  |  |

**Aggregate (under `--provider test`):** does the subagent render as both inline tool
activity and a transient, durably backed Runtime Process; does it settle and leave the
dock; does an unrelated empty session keep an empty dock; and is the missing sub-second
focus/cancel/deletion window recorded honestly. **Full aggregate (real):** does a
long-running Runtime Process show, focus, cancel, and survive session deletion.

---

_Stop triggers and the Abort/RCA + reporting protocol are in [../RULES.md](../RULES.md)._
