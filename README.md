# drengr-hands

[![CI](https://github.com/SharminSirajudeen/drengr-hands/actions/workflows/rust-ci.yml/badge.svg)](https://github.com/SharminSirajudeen/drengr-hands/actions/workflows/rust-ci.yml)
[![Crates.io](https://img.shields.io/crates/v/drengr-hands.svg)](https://crates.io/crates/drengr-hands)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Eyes and hands for AI agents on Android and iOS.**

Your agent already has a brain. This gives it a device.

```bash
drengr look                    # see the screen: numbered elements + screenshot
drengr do tap --element 5      # act: tap / type / swipe / key
drengr query setup --headless  # boot an emulator or simulator if none is running
```

No API key. No account. No cloud. It talks to a local device over ADB (Android)
or `simctl` + a 479-line Swift runner (iOS) — no Appium, no WebDriverAgent required.

## Drive it from your MCP client

```bash
claude mcp add drengr -- drengr mcp
```

Three tools — `drengr_look`, `drengr_do`, `drengr_query`. The loop is: look once,
then `do` repeatedly. Every `do` returns what changed — new elements, navigation,
crashes, network calls — so your agent knows what its last action actually did.

Your agent is the brain. It never needs a key of its own, because the decision
happens in the model you are already paying for.

## Or let it drive itself

```bash
export OPENAI_API_KEY=...        # or GEMINI / ANTHROPIC / GROQ / TOGETHER / FIREWORKS
drengr run --app com.example.app --task "log in with test@test.com"
```

Seven providers, plus `DRENGR_BASE_URL` for any OpenAI-compatible endpoint
(OpenRouter, LiteLLM, vLLM). Ollama runs it with no key and no account:

```bash
ollama pull qwen2.5vl:7b
export DRENGR_VISION_PROVIDER=ollama
drengr run --app com.example.app --task "add the first item to the cart"
```

The model has to accept images and emit strict JSON — the loop escalates to
vision when a screen is underlabelled. `qwen2.5vl:7b` is the tested default;
text-only or thinking-style models will fail to produce a parseable decision.

Simple instructions that name a visible element resolve without any model call
at all, so a lot of a run costs nothing.

It is your key and your bill, always. Nothing is proxied through us.

## Or a device you don't own

```bash
drengr run --cloud browserstack --device "Pixel 8" --os-version 14 \
  --app com.example.app --task "log in"
```

`--cloud` takes `browserstack`, `saucelabs`, `aws`, `lambdatest`, `perfecto`,
`kobiton`, or **any Appium hub URL** — a self-hosted grid, a vendor not on that
list, or `http://localhost:4723`. The named ones only exist so their hub URL and
credential env vars are filled in for you; underneath they are all the same
WebDriver path, so a provider we have never heard of works the same way.

## Tests in CI

```yaml
# drengr-tests.yml
app: com.example.app
tasks:
  - name: checkout
    task: add an item to the cart and reach the payment screen
```

```bash
drengr test drengr-tests.yml --format json
```

## How it works

Four steps, and only one of them needs a model:

| | |
|---|---|
| **Observe** | screenshot + accessibility tree, via ADB or the iOS runner |
| **Orient** | diff against the last screen — what appeared, what navigated, what crashed |
| **Decide** | your agent, or `drengr run`'s built-in loop |
| **Act** | tap, type, swipe, launch, deep-link, set location, grant permissions |

A screen costs ~300 tokens as a text scene instead of ~100KB as an image, so an
agent can watch a whole flow without burning its context on pixels.

## Install

```bash
cargo install drengr-hands
```

Needs the Android SDK platform-tools for Android, and Xcode for iOS.
`drengr doctor` tells you what is missing.

## Built by Drengr Analytics

Drengr Analytics is zero-instrumentation mobile analytics — it understands what
your app does without you writing tracking code, and redacts PII on the device
before anything leaves it. This is the actuation layer, open-sourced.
[drengr.dev](https://drengr.dev)

MIT.
