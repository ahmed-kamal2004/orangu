# bash completion for orangu
#
# Install:
#   source /path/to/orangu.bash
# or drop it into a directory scanned by bash-completion, e.g.
#   ~/.local/share/bash-completion/completions/orangu

_orangu() {
    local cur prev
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    COMPREPLY=()

    case "$prev" in
        -c|--config)
            # Configuration file (orangu.conf)
            COMPREPLY=( $(compgen -f -- "$cur") )
            compopt -o filenames 2>/dev/null
            return 0
            ;;
        -w|--workspace)
            # Workspace root: directories only
            COMPREPLY=( $(compgen -d -- "$cur") )
            compopt -o dirnames 2>/dev/null
            return 0
            ;;
        -r|--resume)
            # Session UUID: scan ~/.orangu/sessions, newest first
            local sessions_dir="${HOME}/.orangu/sessions"
            if [[ -d "$sessions_dir" ]]; then
                local uuids
                uuids=$(command ls -1t "$sessions_dir" 2>/dev/null)
                COMPREPLY=( $(compgen -W "$uuids" -- "$cur") )
            fi
            return 0
            ;;
    esac

    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W \
            "-c --config -w --workspace -r --resume -h --help" -- "$cur") )
        return 0
    fi
}

complete -F _orangu orangu
