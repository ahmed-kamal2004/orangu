\newpage

# Terminal interface

`orangu` is an interactive terminal client with a persistent header and a prompt area anchored to the bottom of the terminal.

## Header

The top banner displays:

- current version
- workspace
- active llama.cpp endpoint
- active model
- `/help` reminder

## Prompt area

The prompt area stays at the bottom of the terminal window.

- Long input wraps upward
- The model name is right-aligned below the prompt frame
- Submitted input moves directly into the output area

## Waiting state

While the model is generating a response, the output area shows a blinking:

```text
Thinking
```

placeholder in the position where the reply will appear.

## History and navigation

Command history is stored in:

```text
~/orangu/orangu.history
```

Use:

- `<ARROW_UP>` to move backward in history
- `<ARROW_DOWN>` to move forward in history

## Editing keys

The prompt supports standard shell-style editing:

- `Ctrl+A`
- `Ctrl+E`
- `Ctrl+K`
- `Ctrl+U`
- `Ctrl+W`
- `Home`
- `End`
- `Left`
- `Right`
- `Tab` completion for slash commands and `/model`

Press `Ctrl+C` once to arm quit mode. Press it again within 2 seconds to exit.
