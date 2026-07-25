# Robottles

It's a robot that takes a bottle (task) off the wall and tells Claude to do it. 

Like in that song, 99 bottles of beer on the wall, except this time it's tasks and the wall is a metaphorical wall (of tasks). 

And thus robot + bottles = robottles. 

## What does it do? 

Connect it to a CalDAV calendar, with tasks, and it will pick up the highest priority incomplete task and tell Claude: "oy, do it, then mark it as complete, then push the new changes to a new branch".

That's pretty much it. 
Have it running on a schedule, have it run whenever you have spare compute or tokens or whatever.

Unhappy with what the agent wrote? 
Good, that means you read its output.
Now go fix it. 
Preferably manually, but I can't tell you what to do, you're not an LLM. Yet.

Doesn't have to be a CalDAV task. Or Claude.

Agents are pluggable, behind an `AgentRunner` trait, configured via the `project.agent` key.
Besides `claude` (the default) and `noop` (does nothing, for dry runs), there's `lmstudio`, which drives a local [LM Studio](https://lmstudio.ai/) server instead: it talks to LM Studio's OpenAI-compatible `/chat/completions` endpoint and gives the model tool-calling access to read/write files and run shell commands in the project checkout, looping until the model replies without any further tool calls.
See `config.yaml.example` for its settings (`base_url`, `model`, `max_iterations`, `timeout_secs`).

Task suppliers are pluggable behind a `TaskSource` trait (`get_next_task`/`mark_completed`), configured via the `source.type` key in `config.yaml`.
We have:
- CalDAV tasks
- Jira tickets
- GitHub issues
- Dummy noop

Contribute new suppliers to your heart's content! 

## Multiple targets

`config.yaml` can define several named `targets`, each with its own task source, agent and project folder. Pick which one to run against by name on the command line:

```
robottles config.yaml project2
```

A single run only ever works on one task in one target's project folder, even if several targets are configured. If the config only has one target, the name can be omitted. See `config.yaml.example` for the full format.

## Loop mode

By default robottles does one task and exits. Set `execution.mode: loop` in `config.yaml` to instead have it run forever: each iteration picks a task for the next configured target (cycling round-robin through all of them), then sleeps `execution.delay_secs` (default 300, i.e. 5 minutes) before moving on. It keeps going until manually stopped (e.g. Ctrl-C). A failure on one iteration is logged and doesn't stop the loop.

```yaml
execution:
  mode: loop
  delay_secs: 300
```

## Running in Docker

A `Dockerfile` and `docker-compose.yml` are provided for running robottles in loop mode as a long-lived container. The image bundles the `claude` CLI (the default agent), so it's ready to go out of the box.

```
git clone https://github.com/morosanmihail/robottles.git
cd robottles
```

Set up `config.yaml` with `execution.mode: loop` (see `config.yaml.example`), and make sure every `project.path` points somewhere under `/projects` (e.g. `/projects/myrepo`) — that's where `docker-compose.yml` mounts a `./projects` host folder into the container. Then:

```
docker compose up -d --build
```

`config.yaml` is bind-mounted read-only into the container (`./config.yaml:/app/config.yaml:ro`), so it's edited on the host — restart the container to pick up changes. The `claude` CLI's login/session state is kept in a named volume (`claude-config`) so it survives restarts; alternatively, set `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` in a `.env` file next to `docker-compose.yml`.

## Installation

### Build from source

Clone the repo and build with Cargo:

```
git clone https://github.com/morosanmihail/robottles.git
cd robottles
cargo build --release
```

The binary will be at `target/release/robottles`.

### Install directly via `cargo install`

No need to clone first, `cargo install` can pull straight from the GitHub repo:

```
cargo install --git https://github.com/morosanmihail/robottles.git
```

This installs the `robottles` binary to `~/.cargo/bin` (make sure that's on your `PATH`).

