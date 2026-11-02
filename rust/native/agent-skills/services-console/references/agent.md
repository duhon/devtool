# Agent panel

The transcript shows user messages, final assistant answers, errors and stop
notices. Intermediate assistant updates and streamed partial messages appear
in that chat's Debug under progress, not as separate transcript entries.
Only the final completed nonempty answer is published after a successful turn;
interrupted drafts remain in Debug. Empty messages are never displayed.

Top tabs are independent chats, not service tabs, and use the same Console tab
style. The + button creates a chat. Double-click a tab to rename it directly in
the tab: Enter or clicking elsewhere saves, Escape cancels. There is no
separate name field above the transcript. Names, drafts and transcripts persist locally.
Changing services or closing the panel preserves the chat and does not stop it.
The tab's x closes that tab and stops its running Agent turn, revoking pending
run approvals. It preserves stored chat history and the SDK session; closed
tabs do not reopen automatically. Closing the last tab creates an empty chat;
empty chats are not kept. The # tab, left of the chats, lists every open and
closed chat of all sessions with its session, message count and last activity,
newest first. Close closes an open chat; Restore reopens a closed chat with its
history in the current session, moving it there. Delete immediately removes
its messages and Copilot history from this computer, without confirmation.

Console context is provided automatically for each message; there is no sharing
checkbox or separate context-access approval. Enter sends a chat message;
Shift+Enter adds a line. There is no Send button or composer placeholder.
Settings → Agent contains a searchable Copilot model input with the model's category
and a Reasoning slider with one step per level the model supports. These
defaults apply only to newly created Agent chats; existing chats retain their
model and Reasoning, including after reload.
Without saved preferences, new chats use Copilot Fusion, which manages Reasoning
automatically. Fusion is a router, not an individual model.
Every reply gets the skill of each installed module (`module-<id>`) and the
skills of the applied sources (`<source>-<name>`). They apply from the
next reply in every chat and are used through the `skill` tool. Only the
SKILL.md text is available: you cannot read other files of a skill.
Assistant responses render Markdown; user messages remain literal text.
Running a check through the Agent always requires a separate Approve run action;
Deny rejects it. Approval applies only to the displayed check/revision and
resolved target, not future actions.
