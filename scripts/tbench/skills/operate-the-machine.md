---
name: operate-the-machine
description: When the task is to configure, install, serve, or administer a system, the machine you are on IS the target; converge its live state and verify.
triggers:
  - configure
  - server
  - webserver
  - install
  - port
  - service
  - daemon
  - ssh
---
When a task asks you to configure a server, set up a service, or make something reachable on a
port, the machine you are running on IS that server. "server", "localhost" and "my computer" in
the task all resolve to this machine unless the task names a remote host you cannot reach.

- You are root here. A missing tool is not a blocker: install it
  (`apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y <pkg>`), then continue.
- The deliverable is LIVE STATE: packages installed, daemons running, ports listening, hooks in
  place. A setup script or a README is a failure unless the task explicitly asks for a document.
- Do not stop at "prepared": start the services and leave them running.
- Before finishing, VERIFY by running the task's own acceptance commands yourself (the clone, the
  push, the curl). If a step needs an interactive password, script it (e.g. expect, sshpass, or
  ssh keys you install). Fix what fails and re-verify until the commands actually succeed.
- If the task says the user will handle a part (e.g. "I'll set up login"), still make the server
  side of that part work end to end for a local equivalent so you can verify the rest.

Lifetime rule, the one that loses tasks: your `bg` tool's jobs are children of YOUR process and
die when you finish. Anything that must still be running AFTER you are done (a web server, sshd,
any listener the user will hit later) must be a real daemon, never a `bg` job:
- prefer the packaged daemon (`apt-get install -y nginx` then `service nginx start` or
  `nginx` directly), configured to serve the right root and port;
- otherwise fully detach it through the shell: `setsid nohup <cmd> >/var/log/x.log 2>&1 < /dev/null &`.
Then prove the survival, not just the response: check the listener's parent is init
(`ps -o ppid= -p <pid>` is 1) and that `curl` still succeeds, before you finish.
