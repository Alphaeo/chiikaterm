# chiika shell integration for zsh, step 1: chiika points ZDOTDIR here. Loads the user's .zshenv
# with their ZDOTDIR, then comes back here so that zsh reads chiika's .zshrc.
__chiika_zdotdir=$ZDOTDIR
ZDOTDIR=${CHIIKA_USER_ZDOTDIR:-$HOME}
[[ -f $ZDOTDIR/.zshenv ]] && . $ZDOTDIR/.zshenv
__chiika_user_zdotdir=$ZDOTDIR
ZDOTDIR=$__chiika_zdotdir
