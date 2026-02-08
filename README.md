# signal-tui

Tiny Rust TUI on top of your existing `signal-cli` config (no extra login).

## License

MIT (see `LICENSE`).

## Run

```bash
cargo run
```

Optional:

```bash
cargo run -- --account +15551234567
cargo run -- --signal-cli /home/rob/.local/bin/signal-cli
```

## Config + Scrollback

On first run, `signal-tui` creates:

- Config: `~/.config/signal-tui/config.toml` (or `$XDG_CONFIG_HOME/signal-tui/config.toml`)
- Scrollback (saved chat history): `~/.local/state/signal-tui/scrollback/` (or `$XDG_STATE_HOME/signal-tui/scrollback/`)

Scrollback is stored as JSONL (one JSON object per line) per conversation.

## Keys

- `j`/`k` (or arrows): move
- `a`: add recipient (`+E164`, e.g. `+15551234567`)
- `i`: compose message
- `Enter`: send (in insert mode)
- `Esc`: cancel (insert/add-recipient)
- `r`: sync once (in addition to background receive)
- `q`: quit

## Dev: pre-commit

This repo uses `pre-commit` to catch common issues before commits, including secret scanning (gitleaks) and a guard that fails commits if you have untracked or unstaged files you probably forgot to include.

Install + enable:

```bash
python3 -m venv .venv
.venv/bin/pip install pre-commit
.venv/bin/pre-commit install
```

Run on demand:

```bash
.venv/bin/pre-commit run --all-files
```
