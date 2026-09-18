# Drengr

**Eyes and hands for AI agents on mobile devices.**

Drengr is an MCP server that gives AI agents (Claude, GPT, Gemini) the ability to see, tap, type, and navigate mobile apps — Android and iOS.

---

## Install

```bash
npm install -g drengr
```

Or run without installing:

```bash
npx drengr
```

Or via shell script:

```bash
curl -fsSL https://drengr.dev/install.sh | bash
```

After install, run `drengr build-runner` once (interactive terminal) to build the iOS runner. Re-run after iOS or Xcode upgrades.

---

## Quick Setup

```bash
# Check your system
drengr doctor

# Configure your MCP client (Claude Desktop, Claude Code, Cursor, etc.)
drengr setup
```

## Add to Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "drengr": {
      "command": "drengr",
      "args": ["mcp"]
    }
  }
}
```

## Add to Claude Code

```bash
claude mcp add drengr -- drengr mcp
```

## Add to Cursor

Add to `.cursor/mcp.json` in your project:

```json
{
  "mcpServers": {
    "drengr": {
      "command": "drengr",
      "args": ["mcp"]
    }
  }
}
```

---

## What Drengr does

- **3 MCP tools**: `drengr_look` (observe screen), `drengr_do` (take action), `drengr_query` (ask questions)
- **Android & iOS** — ADB for Android, simctl for iOS, Appium for cloud devices
- **Vision-first** — sees your app like a human via annotated screenshots
- **Text mode** — ~300 tokens per screen instead of 100KB images
- **Situation reports** — after every action: what changed, what's new, is it stuck?
- **Screen exploration** — auto-maps your app's navigation graph
- **Network capture** — sees HTTP calls during each action

---

## Supported platforms

| Platform | Architecture | Supported |
|----------|-------------|-----------|
| macOS    | arm64 (M1+) | ✓         |
| macOS    | x64         | ✓         |
| Linux    | x64         | ✓         |
| Linux    | arm64       | ✓         |

---

## Uninstall

```bash
drengr uninstall           # removes data + MCP configs
npm uninstall -g drengr    # removes the npm package
```

Removes the binary, all local data (`~/.drengr/`), and cleans drengr from your MCP client configs. Leaves nothing behind.

---

## Documentation

Full documentation and setup guide at [drengr.dev/getting-started](https://drengr.dev/getting-started)

## License

MIT — source at https://github.com/SharminSirajudeen/drengr-hands
See [LICENSE](LICENSE) for details.
