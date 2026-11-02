# pyharness

pyharness is a coding and research agent for general and long-running work,
built around two ideas:

- The **[Recursive Language Model (RLM)](https://www.primeintellect.ai/blog/rlm)** treats context as variables (*prompt-as-a-variable*) and tools like recursive subagents as function calls (*programmatic tool and sub-agent calling*) inside a persistent REPL.
- The **[Continual Harness](https://arxiv.org/abs/2605.09998)** stores supplemental prompts, memories, skill descriptions, and reusable subagent specifications as durable state that the agent can refine through small, evidence-backed updates, local to the session by default.

pyharness is a fork of [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent)
by Prime Intellect. It differs from upstream in a few ways:

- **No sign-in screen.** It opens straight into a session. Set up a model with
  `/login` or a provider API key environment variable.
- **No usage telemetry is sent anywhere.** Telemetry is off by default, and
  turning it on only writes a local log (`~/.pyharness/telemetry.jsonl`).
- **Session transcripts are never uploaded.** Trace sharing is removed.
- **No built-in update server.** `pyharness update` only works with a release
  server you configure (`PYHARNESS_DOWNLOAD_BASE_URL`).
- Configuration and sessions live in `~/.pyharness`.

Prime Inference is still available as one of the model providers.

## Install

Build from source (see the [install and setup guide](docs/install.md) for
requirements, connecting a model, updating and troubleshooting):

```bash
git clone https://github.com/thattaway/pyharness
cd pyharness
cargo build --release -p ph-cli --bin pyharness
ln -sf "$PWD/target/release/pyharness" ~/.local/bin/pyharness
```

Keep the clone in place: the binary loads its Python runtime and built-in
skills from it.

## Why pyharness

pyharness combines a persistent Python control environment with durable harness state, so useful working context and reusable operating patterns can outlive a single chat window.

- **Everything is programmatic:** a persistent Python REPL is the built-in model tool; file operations, shell commands, tool use, subagents, and context management happen through code.
- **Subagents are built in:** `rlm.spawn(...)` spawns real child agents for parallel or background work and returns their results programmatically.
- **The harness can improve:** `/refine` reviews the current trajectory and can apply small, evidence-backed updates to supplemental harness state. It never rewrites the immutable base system prompt, and recorded snapshots support rollback.
- **Skills are executable:** skills are importable Python packages, and the built-in skill creator can turn recurring workflows into project or personal skills.
- **Sessions run in the background:** daemon-backed agents keep running when the terminal disconnects and can be reattached later.
- **Agents communicate directly:** running agents can exchange messages and orchestrate one another without routing everything through the user.
- **Long tasks keep moving:** automatic compaction, persistent goals, heartbeats, schedules, autonomous mode, and retained subagents preserve progress across turns and terminal sessions.

## Slack

Connect Slack from `/mcp` or run `/mcp login slack` inside pyharness to search
your workspace and use Slack tools. See the [Slack connector guide](docs/slack-mcp.md)
for OAuth setup and troubleshooting.

## Google Drive and Google Sheets

Connect Google's official Drive and Sheets MCP servers with your own Google
OAuth client: add them to `mcpServers` with `clientId`, `clientSecret`,
`callbackPort`, and `scopes`, then run `/mcp login <name>`. See the
[Google Drive and Google Sheets guide](docs/google-workspace-mcp.md) for the
Google Cloud setup and the tool mapping.

## Clipboard over SSH and tmux

pyharness uses the local clipboard tool (such as `pbcopy`) when available. In tmux it asks the attached tmux client to forward a clipboard buffer if that client advertises the `Ms` capability. This works with tmux's default `set-clipboard external`, unlike application-origin OSC 52. Without tmux, remote copies request delivery using OSC 52. Terminal requests cannot confirm that your local clipboard changed, so pyharness reports them as unconfirmed.

Nested tmux servers and terminals may still block delivery. An outer tmux with `set-clipboard external` can reject the inner server's OSC 52. In that case, run `tmux set -s set-clipboard on` on the outer server, or use native terminal selection (Shift-drag in most terminals; Option-drag in iTerm2) followed by Cmd-C. macOS Terminal.app does not support OSC 52; iTerm2 requires its “Applications in terminal may access clipboard” setting.

## Custom certificate authorities

HTTPS requests trust your operating system's trust store together with the bundled Mozilla roots. For servers behind a custom CA, point `NODE_EXTRA_CA_CERTS` at a PEM bundle of your CA certificates — the same variable the Node/TypeScript build honors:

```bash
export NODE_EXTRA_CA_CERTS=/path/to/your-ca.pem
```

Each transport reads the variable once when its first client is initialized. The background daemon keeps the environment it started with, so run `pyharness shutdown` after changing it.

## Getting Started

Start pyharness from the repository or directory you want it to work in:

```bash
cd /path/to/project
pyharness
```

On first launch, run `/login` to choose a subscription or API-key provider. pyharness works in the current directory and can run commands and modify files there. Use a disposable clone, clean worktree, or another checkpoint you can inspect and restore.

> [!WARNING]
> pyharness executes model-generated Python and project commands with your user permissions. Its worker and kernel processes improve lifecycle isolation and recovery; they are **not** a security sandbox. Review changes and use trusted repositories, instructions, and skills only. Run untrusted code or instructions in an external sandbox or restricted environment.

Useful commands:

```bash
pyharness agents                   # Browse running, idle, and saved sessions
pyharness attach <agent>           # Reattach to a running session
pyharness --resume [path|id]       # Browse sessions or resume one directly
pyharness status                   # Inspect background service state
pyharness doctor [--fix]           # Inspect or repair background services
pyharness update [--force]         # Update from your configured release server
pyharness shutdown [--force]       # Stop every agent, worker, and background service
```

## Built for Long-Running Work
pyharness is built for long-running work, especially for evaluations in research. These features are available in the TUI, and when run autonomously.

- **Continual Harness:** `/refine` can persist focused, reviewable lessons as supplemental prompts, memories, reusable skill descriptions, or subagent specifications, with recorded refinement history. It does not replace packaging and reviewing new executable skills.
- **Direct agent-to-agent communication:** running agents and retained subagents can discover one another, exchange messages, and steer active work.
- **Daemon-backed continuity:** active sessions, Python REPL state, schedules, and subagents keep running when the terminal detaches and can be reattached later.
- **Heartbeats and schedules:** `/heartbeat`, `rlm_heartbeat`, and `pyharness schedule` can re-enter a session periodically or at a specific time.
- **Persistent goals:** `/goal` keeps an objective and its progress active across turns until it is completed, paused, or cleared.
- **Bounded autonomous mode:** `/autonomous` continues within configured turn, token, and time budgets and can run user-defined quality gates. A passed gate checks only what that gate verifies; reaching a limit does not imply task success.

## Contributing

Open an issue or pull request on [thattaway/pyharness](https://github.com/thattaway/pyharness).

## License

pyharness is released under the [MIT License](LICENSE), as is the upstream
Prime Agent code it is built from.

## Citation

pyharness is built on Prime Agent. If you use this codebase in your research, please cite the Prime Agent paper:

```bibtex
@article{karten2026prime,
  title={Prime Agent: A Self-Improving RLM Harness},
  author={Karten, Seth and Zhang, Alex L. and Thomas, Kevin and Müller, Sebastian and Bakouch, Elie and Auras, Daniel and Senghaas, Mika and Obeid, Fares and Dunas, Konstantin and Hagemann, Johannes and Jaghouar, Sami},
  journal={arXiv preprint arXiv:2608.23552},
  year={2026}
}
```

Available at [https://arxiv.org/abs/2608.23552](https://arxiv.org/abs/2608.23552).

### Automatic compaction settings

`/settings` exposes the global automatic compaction percentage in five-point steps,
with a 95% default. A saved off-grid percentage stays selectable. The effective
trigger also reserves room for retained history and summaries; the model's input
and requested-output ceiling always takes priority. Manual compaction budgets are
unchanged. The underlying `compaction.thresholdPercent` setting accepts 1–100,
including fractional values; project settings can override the global setting.

For per-model early compaction, use the existing provider-scoped
`modelOverrides.<id>.contextWindow` option in `models.json`, for example:

```json
{
  "providers": {
    "prime-inference": {
      "modelOverrides": {
        "your-model-id": { "contextWindow": 524288 }
      }
    }
  }
}
```

Use the actual model ID for your provider. This sets that model's effective context
window; it does not change every model's global compaction percentage.
