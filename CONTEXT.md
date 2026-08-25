# lash-cli Context

## Interaction Glossary

- **Host Application**: A deployable product that selects Lash runtime crates, providers, plugins, Execution Modes, persistence, configuration, and presentation. It owns those composition choices and may release independently from Lash.
- **Operator UI**: The terminal interaction contract presented by the `lash` Host Application.

## Operator UI

- `Ctrl+C` is reserved for cancel/dismiss/quit semantics: close suggestions or overlays, cancel an active turn, clear a non-empty draft, then quit only from an idle empty prompt.
- Copy uses `Ctrl+Shift+C` by default. `Ctrl+U` deletes draft text to the start of the line, `Ctrl+K` deletes to the end, and history/document scrolling uses PgUp/PgDn, mouse wheel, and scroll gestures.
- `Ctrl+P` opens the command and settings palette — a searchable overlay of settings actions (theme, model, variant, and other commands) applied directly without typing a slash command.
- The status bar shows model and reasoning variant joined, then execution mode and the pinned RLM dialect when applicable, for example `gpt-5.5 medium · rlm · typescript`; it carries no `lash` brand prefix. Context usage is labeled as `ctx`.
- Queue previews sit directly above the input. Early-injected work is labeled `Will send in this turn`; next-turn work is labeled `Queued for next turn`.
- Escape during an active turn durably cancels that exact turn with dropped-input disposition, restores affected steer text ahead of the current editor draft, and never replays it automatically on a later turn. A resumed session surfaces any dropped text that could not be restored before exit as a system message.
- The `/resume` picker discovers live root sessions from Lash's unified catalog and uses the host roster only for display metadata. It hides zero-turn sessions when any non-empty session exists. If only empty sessions exist it shows them with `No messages yet`; catalog-only sessions receive fallback labels, and direct `/resume <id-or-name>` may still target any session. Alpha-era per-session databases add the notice `N sessions from an older Lash are not openable` and are never opened.
- The current durable compatibility markers are session schema 41, trace schema 9, and remote protocol 46. `lash --version` and `/info` report these markers.

## Autonomous CLI Testing

- Agent-driven e2e runbooks live in `runbooks/` (start with `runbooks/RULES.md`): scenario documents an agent executes through `scripts/lash-operator.py`, judging CLI semantics against this file's Operator UI contracts. The scripted CLI E2E harness remains deterministic gate evidence; runbooks are the judged layer on top.
- Run `cargo test -p lash-cli --features test-provider --test cli_e2e` to exercise the real `lash` binary without live provider credentials.
- The PTY smoke test launches interactive mode with a deterministic `test` provider, types a prompt, waits for rendered output, exits with `/exit`, and validates the generated UI trace/snapshot.
- Run `scripts/lash-operator.py --provider test` for an agent-operated PTY session. It builds/launches the real `lash` binary with an isolated deterministic provider, then accepts commands such as `expect 15 Idle`, `type hello`, `key enter`, `screen`, and `lash-exit`.
- Run `scripts/lash-operator.py --provider real -- --model <provider/model>` to drive Lash against the user's configured provider/API credentials. Use this deliberately because it may spend real model tokens.
