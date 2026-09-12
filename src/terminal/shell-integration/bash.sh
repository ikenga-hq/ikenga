#!/usr/bin/env bash
# Ikenga OSC 133 Shell Integration for Bash
# Implements semantic prompt marking conforming to the FinalTerm / FTCS OSC 133 specification:
#   OSC 133 ; A ST           - Prompt start
#   OSC 133 ; B ST           - Command start (prompt end, input line begin)
#   OSC 133 ; C ST           - Command executed / output start
#   OSC 133 ; D [; <code>] ST - Command finished with exit code

if [ -z "$__IKENGA_BASH_LOADED" ]; then
    __IKENGA_BASH_LOADED=1
    __ikenga_cmd_running=0
    __ikenga_in_prompt_cmd=0

    __ikenga_prompt_cmd() {
        local exit_code=$?
        __ikenga_in_prompt_cmd=1
        if [ "$__ikenga_cmd_running" = "1" ]; then
            printf "\033]133;D;%d\007" "$exit_code"
            __ikenga_cmd_running=0
        fi
        printf "\033]133;A\007"
        __ikenga_in_prompt_cmd=0
    }

    __ikenga_preexec() {
        if [ "$__ikenga_in_prompt_cmd" = "1" ]; then
            return
        fi
        if [ "$BASH_COMMAND" = "__ikenga_prompt_cmd" ]; then
            return
        fi
        if [ "$__ikenga_cmd_running" = "0" ]; then
            __ikenga_cmd_running=1
            printf "\033]133;C\007"
        fi
    }

    if [ -n "$PROMPT_COMMAND" ]; then
        PROMPT_COMMAND="__ikenga_prompt_cmd; $PROMPT_COMMAND"
    else
        PROMPT_COMMAND="__ikenga_prompt_cmd"
    fi

    PS1="$PS1\[\033]133;B\007\]"
    trap "__ikenga_preexec" DEBUG
fi
