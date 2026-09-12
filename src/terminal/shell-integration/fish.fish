# Ikenga OSC 133 Shell Integration for Fish
# Implements semantic prompt marking conforming to the FinalTerm / FTCS OSC 133 specification

if not set -q __IKENGA_FISH_LOADED
    set -g __IKENGA_FISH_LOADED 1
    set -g __ikenga_cmd_running 0

    function __ikenga_on_prompt --on-event fish_prompt
        set -l exit_code $status
        if test "$__ikenga_cmd_running" = "1"
            printf "\e]133;D;%s\a" "$exit_code"
            set -g __ikenga_cmd_running 0
        end
        printf "\e]133;A\a"
    end

    function __ikenga_on_preexec --on-event fish_preexec
        set -g __ikenga_cmd_running 1
        printf "\e]133;C\a"
    end

    if functions -q fish_prompt
        functions -c fish_prompt __ikenga_original_fish_prompt
        function fish_prompt
            __ikenga_original_fish_prompt
            printf "\e]133;B\a"
        end
    end
end
