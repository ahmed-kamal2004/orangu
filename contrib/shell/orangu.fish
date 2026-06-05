# fish completion for orangu
#
# Install: copy this file to ~/.config/fish/completions/orangu.fish
# (fish loads completions from there automatically).

# Completes session UUIDs from ~/.orangu/sessions, newest first.
function __orangu_sessions
    set -l sessions_dir "$HOME/.orangu/sessions"
    test -d "$sessions_dir"; or return
    path basename (path sort --reverse --key=mtime $sessions_dir/*/)
end

complete -c orangu -s c -l config    -r                                    -d 'Path to the configuration file (orangu.conf)'
complete -c orangu -s w -l workspace  -x -a '(__fish_complete_directories)' -d 'Workspace root for local tools'
complete -c orangu -s r -l resume     -x -a '(__orangu_sessions)'           -d 'Resume a session by UUID'
complete -c orangu -s h -l help                                            -d 'Print help'
