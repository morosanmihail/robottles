# Robottles

It's a robot that takes a bottle (task) off the wall and tells Claude to do it. 

Like in that song, 99 bottles of beer on the wall, except this time it's tasks and the wall is a metaphorical wall (of tasks). 

And thus robot + bottles = robottles. 

## What does it do? 

Connect it to a CalDAV calendar, with tasks, and it will pick up the highest priority incomplete task and tell Claude: "oy, do it, then mark it as complete, then push the new changes to a new branch".

That's pretty much it. 
Have it running on a schedule, have it run whenever you have spare compute or tokens or whatever.
Maybe I'll extend it to support local LLMs. 
Well, not maybe, surely. 

Task suppliers are pluggable behind a `TaskSource` trait (`get_next_task`/`mark_completed`), configured via the `source.type` key in `config.yaml`. CalDAV is one implementer; there's also a `dummy` source (always hands back a single "make no changes" task) useful for trying things out.
Contributing a new supplier is just adding another `TaskSource` implementer.

## Multiple targets

`config.yaml` can define several named `targets`, each with its own task source, agent and project folder. Pick which one to run against by name on the command line:

```
robottles config.yaml project2
```

A single run only ever works on one task in one target's project folder, even if several targets are configured. If the config only has one target, the name can be omitted. See `config.yaml.example` for the full format.

