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

