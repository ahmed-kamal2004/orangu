\newpage

# Usage tools

The usage tools cover the session-level housekeeping commands: inspecting how much the current session has used, clearing the conversation, and leaving the client. Like the other local commands, they are handled locally and keep working even when the server or model status in the header is red.

\newpage

## /usage

Shows usage statistics for the current session.

The report covers:

- total application time,
- total time spent waiting for LLM responses,
- total tokens generated (counted with the bundled tokenizer), and
- average tokens per second.

### Examples

```text
/usage
```

Natural-language forms:

```text
usage
show usage
```

\newpage

## /clear

Clears the current conversation.

The in-memory conversation history is dropped so the next prompt starts a fresh exchange. The session itself is preserved — only the conversation context is cleared. (To also return to the configured model and server, use `/reload`.)

### Examples

```text
/clear
```

Natural-language forms:

```text
clear
clear conversation
reset conversation
```

\newpage

## /quit

Exits the client.

`/quit` exits immediately, whereas `Ctrl+C` uses a two-step confirmation (press it once to arm quit mode, then again within two seconds to exit). On exit the full resume command is printed — for example:

```text
orangu --resume 550e8400-e29b-41d4-a716-446655440000
```

so you can return to exactly this session later. The resume command is not printed when the session had no LLM interaction (zero tokens generated) and was on `main`, `master`, or outside a Git repository; in that case the session directory is deleted silently. See the Sessions section of the Terminal interface chapter for the full cleanup rules.

### Examples

```text
/quit
```

Natural-language forms:

```text
quit
exit
```
