# Installing and setting up pyharness

pyharness has no published releases, so you build it from source. This takes a
few minutes the first time.

## 1. Requirements

- **Linux or macOS.** Windows builds exist in the code but are untested in this fork.
- **Rust**, a recent stable toolchain. Install it with [rustup](https://rustup.rs):

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

- **git**, and a C toolchain (`build-essential` on Debian/Ubuntu, Xcode
  Command Line Tools on macOS).
- **Internet access on first launch.** pyharness sets up its own Python
  environment the first time it starts (it downloads `uv`, Python and a few
  Python packages). After that it runs offline. You do not need to install
  Python or `uv` yourself.

## 2. Build

```bash
git clone https://github.com/thattaway/pyharness
cd pyharness
cargo build --release -p ph-cli --bin pyharness
```

The binary lands at `target/release/pyharness`.

> **Keep the clone where it is.** A binary built from source finds its Python
> runtime and built-in skills inside this folder. If you move or delete the
> clone, the agent's Python tool stops working.

## 3. Put it on your PATH

Link the binary instead of copying it, so a rebuild is picked up automatically:

```bash
mkdir -p ~/.local/bin
ln -sf "$PWD/target/release/pyharness" ~/.local/bin/pyharness
```

Make sure `~/.local/bin` is on your `PATH` (add
`export PATH="$HOME/.local/bin:$PATH"` to your shell profile if it is not),
then check it:

```bash
pyharness --version
```

## 4. Connect a model

pyharness needs at least one model provider. Pick one of these.

### Option A: sign in from inside the app

Start pyharness and type `/login`. It lists the providers you can sign in to
(subscription logins such as ChatGPT or Claude, and API-key providers).
Credentials are saved in `~/.pyharness/auth.json`.

### Option B: an API key in your environment

Set the provider's key before you start pyharness, for example:

| Provider | Variable |
| --- | --- |
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Google Gemini | `GEMINI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Mistral | `MISTRAL_API_KEY` |
| Groq | `GROQ_API_KEY` |
| xAI | `XAI_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| Prime Inference | `PRIME_API_KEY` |

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

### Option C: your own OpenAI-compatible server

For a local server (llama-swap, llama.cpp, vLLM, Ollama's OpenAI endpoint and
similar), create `~/.pyharness/models.json`:

```json
{
  "providers": {
    "local": {
      "baseUrl": "http://172.16.10.40:8034/v1",
      "apiKey": "none",
      "api": "openai-completions",
      "models": [
        { "id": "ornith_1.5_9B", "name": "Ornith 1.5 9B" }
      ]
    }
  }
}
```

- `baseUrl` is your server's OpenAI-compatible base URL, ending in `/v1`.
- `apiKey` is required. Use any placeholder if your server does not check it.
- Each `id` must match the model name your server expects.

## 5. Start a session

Run pyharness from the folder you want it to work in:

```bash
cd /path/to/project
pyharness
```

Choose or switch models with `/model`. pyharness can run commands and edit
files in this folder with your permissions, so try it on a scratch copy or a
clean git checkout first.

Useful commands:

```bash
pyharness agents          # browse running, idle and saved sessions
pyharness --resume        # pick a saved session to resume
pyharness status          # show the background service state
pyharness doctor --fix    # check and repair the background service
pyharness shutdown        # stop every agent and the background service
```

## Updating

Pull and rebuild, then restart the background service so it runs the new build:

```bash
cd /path/to/pyharness
git pull
cargo build --release -p ph-cli --bin pyharness
pyharness shutdown
```

`pyharness update` only works if you host your own releases and set
`PYHARNESS_DOWNLOAD_BASE_URL`; otherwise it refuses and downloads nothing.

## Where things are stored

Everything lives in `~/.pyharness`:

| Path | What it holds |
| --- | --- |
| `settings.json` | your settings (`/settings` edits it) |
| `models.json` | custom providers and models |
| `auth.json` | saved logins and API keys |
| `sessions/` | saved sessions |
| `kernel-venv/` | the agent's Python environment |

Set `PYHARNESS_CODING_AGENT_DIR` to use a different folder.

**Coming from Prime Agent?** pyharness does not read `~/.prime/agent`. To keep
your settings, logins and sessions, copy them over before the first launch:

```bash
cp -R ~/.prime/agent ~/.pyharness
```

## Privacy

- Telemetry is off by default and is never sent over the network. `/telemetry on`
  only writes a local log (`~/.pyharness/telemetry.jsonl`).
- Session transcripts are never uploaded.
- pyharness only contacts the model providers you configure, GitHub (to download
  the public model list) and, on first launch, the servers `uv` uses to install
  Python.

## Troubleshooting

- **"Failed to set up the Python kernel runtime"**: the first launch needs
  internet access. Check your connection or proxy and start pyharness again.
- **Changes from a rebuild do not show up**: run `pyharness shutdown`, then
  start pyharness again.
- **No models to choose from**: add a provider (step 4), then check `/model`.
- **Something seems stuck**: `pyharness doctor --fix` repairs the background
  service.
