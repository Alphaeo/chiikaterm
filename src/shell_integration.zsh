# chiika shell integration for zsh, step 2 (`.zshrc`): loads the user's .zshrc, then marks prompts
# and commands (OSC 133) so chiika can follow them. The user's files are not touched.
ZDOTDIR=$__chiika_user_zdotdir
[[ -f $ZDOTDIR/.zshrc ]] && . $ZDOTDIR/.zshrc
unset __chiika_zdotdir __chiika_user_zdotdir CHIIKA_USER_ZDOTDIR
__chiika_first=1

# First of precmd: sees the command's status.
__chiika_precmd() {
    local code=$?
    [[ -z $__chiika_first ]] && printf '\e]133;D;%s\a' $code
    __chiika_first=
    printf '\e]9;9;%s\a\e]133;A\a' "$PWD"
}

# Last of precmd: prompts rebuilt by the user's hooks still end with the mark.
__chiika_ps1() {
    [[ $PS1 == *$'%{\e]133;B\a%}' ]] || PS1+=$'%{\e]133;B\a%}'
}

__chiika_preexec() {
    local line=$1
    line=${line//\\/\\\\}
    line=${line//;/\\x3b}
    line=${line//$'\n'/\\x0a}
    line=${line//[$'\e\a\r']/}
    printf '\e]633;E;%s\a\e]133;C\a' "$line"
}

precmd_functions=(__chiika_precmd $precmd_functions __chiika_ps1)
preexec_functions+=(__chiika_preexec)
