#!/usr/bin/env zsh
# Ikenga OSC 133 Shell Integration for Zsh
# Implements semantic prompt marking conforming to the FinalTerm / FTCS OSC 133 specification:
#   OSC 133 ; A ST           - Prompt start
#   OSC 133 ; B ST           - Command start (prompt end, input line begin)
#   OSC 133 ; C ST           - Command executed / output start
#   OSC 133 ; D [; <code>] ST - Command finished with exit code

if [[ -z "$__IKENGA_ZSH_LOADED" ]]; then
    __IKENGA_ZSH_LOADED=1
    __ikenga_cmd_running=0

    __ikenga_precmd() {
        local exit_code=$?
        if [[ "$__ikenga_cmd_running" -eq 1 ]]; then
            printf "\033]133;D;%d\007" "$exit_code"
            __ikenga_cmd_running=0
        fi
        printf "\033]133;A\007"
    }

    __ikenga_preexec() {
        __ikenga_cmd_running=1
        printf "\033]133;C\007"
    }

    autoload -Uz add-zsh-hook
    add-zsh-hook precmd __ikenga_precmd
    add-zsh-hook preexec __ikenga_preexec

    PS1="${PS1}%{\033]133;B\007%}"
    PROMPT="${PROMPT}%{\033]133;B\007%}"
fi
