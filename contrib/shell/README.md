# Shell completions

Command-line completion scripts for the `orangu` binary. They complete the
startup flags and their arguments:

| Short | Long          | Completion        |
| ----- | ------------- | ----------------- |
| `-c`  | `--config`    | files             |
| `-w`  | `--workspace` | directories       |
| `-r`  | `--resume`    | session UUIDs from `~/.orangu/sessions` (newest first) |
| `-h`  | `--help`      | —                 |

| Shell | File           |
| ----- | -------------- |
| bash  | `orangu.bash`  |
| zsh   | `_orangu`      |
| fish  | `orangu.fish`  |

## bash

Source the script from your `~/.bashrc`:

```sh
source /path/to/orangu/contrib/shell/orangu.bash
```

Or install it where `bash-completion` looks for per-command scripts:

```sh
install -Dm644 contrib/shell/orangu.bash \
    ~/.local/share/bash-completion/completions/orangu
```

## zsh

Copy the file (it must be named `_orangu`) into a directory on your `$fpath`
and make sure `compinit` runs:

```sh
mkdir -p ~/.zsh/completions
cp contrib/shell/_orangu ~/.zsh/completions/_orangu
```

```sh
# ~/.zshrc
fpath=(~/.zsh/completions $fpath)
autoload -Uz compinit && compinit
```

## fish

fish loads completions from `~/.config/fish/completions/` automatically:

```sh
cp contrib/shell/orangu.fish ~/.config/fish/completions/orangu.fish
```
