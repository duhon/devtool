---
name: module-terminal
description: "Terminal checks in the Services DevTools Console: #target, #payload, #env and how exit codes and stderr set the status. Load before writing, editing or explaining Terminal requests."
---

# Terminal service

Use `#target` with the configured shell or ssh command line, `#payload` for the
command text and optional `#env` for Name: value entries. This is an application
service, not general shell permission for the Agent. Running a Terminal check
still requires the user's explicit request and the separate one-time approval.
Never use it to read credentials, browser storage or application data files.
The response shows stdout then stderr. Exit code 0 is OK; any non-zero exit
code or termination by a signal is ERROR (Stop is Stopped), so `grep` without
matches or `diff` with differences are ERROR too. Exit 0 is WARNING when the
command wrote anything besides whitespace to stderr, or when detached background
processes keep the output open. Debug shows the process answer with its exit
`code` or `signal`, `stdout` and `stderr`.
