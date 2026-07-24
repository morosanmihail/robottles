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

Maybe I'll extend it to support non-CalDAV calendars.
Less certain of that one, but I will reorganise the code manually to allow for configurable task lists, to make it easier for people to contribute their own preferred ones.
Or, let's be honest, have an LLM add support for their favourite. 

