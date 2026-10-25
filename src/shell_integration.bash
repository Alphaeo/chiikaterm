# chiika shell integration for bash: marks prompts and commands (OSC 133) so chiika can follow them.
# Started by chiika with `bash --rcfile <this file> -i`: loads ~/.bashrc first, the user's files
# are not touched. The command line needs PS0 (bash 4.4+).
if [ -n "$CHIIKA_WSL" ]; then
    # wsl.exe starts a login shell: the files `bash -l` reads.
    [ -f /etc/profile ] && . /etc/profile
    for __chiika_f in ~/.bash_profile ~/.bash_login ~/.profile; do
        [ -f "$__chiika_f" ] && { . "$__chiika_f"; break; }
    done
    unset __chiika_f
else
    [ -f ~/.bashrc ] && . ~/.bashrc
fi
__chiika_first=1

# First of PROMPT_COMMAND: sees the command's status, and gives it back to the user's commands.
__chiika_prompt() {
    local code=$? cwd=$PWD
    [ -z "$__chiika_first" ] && printf '\e]133;D;%s\a' "$code"
    __chiika_first=
    # In WSL, the Windows path: chiika starts new shells there.
    [ -n "$CHIIKA_WSL" ] && cwd=$(wslpath -w "$PWD" 2>/dev/null || printf %s "$PWD")
    printf '\e]9;9;%s\a\e]133;A\a' "$cwd"
    return $code
}

# Last of PROMPT_COMMAND: prompts rebuilt by the user's commands still end with the mark.
__chiika_ps1() {
    [[ $PS1 == *'\[\e]133;B\a\]' ]] || PS1+='\[\e]133;B\a\]'
}

# Expanded by PS0, once the line is read and before it runs.
# ponytail: the line comes from the history, so a command HISTCONTROL=ignorespace drops (or
# HISTSIZE=0) shows the previous one; a DEBUG trap if that matters.
__chiika_preexec() {
    local line
    # "  372  make test" (a `*` after the number for an edited entry).
    line=$(HISTTIMEFORMAT= history 1)
    line=${line#"${line%%[![:space:]]*}"}
    line=${line#"${line%%[![:digit:]]*}"}
    line=${line#\*}
    line=${line#"${line%%[![:space:]]*}"}
    line=${line//\\/\\\\}
    line=${line//;/\\x3b}
    line=${line//$'\n'/\\x0a}
    line=${line//[$'\e\a\r']/}
    printf '\e]633;E;%s\a\e]133;C\a' "$line"
}

PROMPT_COMMAND="__chiika_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND};__chiika_ps1"
PS0="${PS0}"'$(__chiika_preexec)'
