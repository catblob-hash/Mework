//! The bash half of the host terminal's command barrier.
//!
//! The protocol is zsh's ([`super::posix_control`]): the same authenticated
//! `ready`/`start`/`end` frames on the terminal output, and a line that runs
//! something is not run until the host answers `start`. Only the answer's route
//! differs by platform: a FIFO on a Mac or Linux host, reply files under Git
//! Bash on Windows, whose MSYS runtime can wait on neither a Win32 event nor a
//! FIFO a native process wrote ([`super::reply_files`]).
//!
//! bash is started as `bash --rcfile <session file> -i`, and the generated file
//! does what a login shell's startup would, in order: the control secrets are
//! taken into shell variables and out of the environment, `/etc/profile` and the
//! first of `~/.bash_profile`, `~/.bash_login` and `~/.profile` are sourced the
//! way `bash -l` sources them, and only then are the hooks installed, so they
//! wrap whatever line editor the user's files built.
//!
//! bash has no way to hold an accepted line, so the hooks live one step
//! earlier, in readline. Every key bound to a command that runs the line —
//! `accept-line`, `operate-and-get-next`, `edit-and-execute-command` — is
//! rebound to a macro of private keys: a `bind -x` gate that asks the host and
//! then rebinds the next key of the same macro either to the original command
//! or to nothing, which leaves the refused line in place so Enter retries it,
//! as zsh does. `PROMPT_COMMAND` reports the end of the command. None of this
//! needs more than bash 3.2, which is what macOS still ships as `/bin/bash`;
//! see the notes on the 3.2 display quirks below.

use super::{DECISION_TIMEOUT_SECONDS, REFUSED_MESSAGE};

/// How a shell learns the host's answer to `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    /// Lines of `a <generation>` or `r <generation>` read from the FIFO named
    /// by `MEWORK_TERMINAL_CONTROL_CHANNEL`.
    #[cfg_attr(not(unix), allow(dead_code))]
    Fifo,
    /// A file `reply-<generation>` holding `a` or `r`, which the host renames
    /// into the directory `MEWORK_TERMINAL_CONTROL_CHANNEL` names.
    #[cfg_attr(not(windows), allow(dead_code))]
    ReplyFiles,
}

/// The rcfile for one session.
pub(super) fn bashrc(decision: Decision) -> String {
    debug_assert!(!REFUSED_MESSAGE.contains('\''));
    let wait = match decision {
        Decision::Fifo => FIFO_WAIT,
        Decision::ReplyFiles => REPLY_FILE_WAIT,
    };
    [SECRETS, wait, HOOKS, STARTUP]
        .concat()
        .replace("@DECISION_TIMEOUT@", &DECISION_TIMEOUT_SECONDS.to_string())
        .replace("@REFUSED_MESSAGE@", REFUSED_MESSAGE)
}

/// The control secrets leave the environment before anything else runs; every
/// function is defined here too, before the user's files, so an alias of theirs
/// (`alias read=…`) cannot rewrite a body when it is parsed.
const SECRETS: &str = r#"# Mework terminal integration, generated for one session.
__mework_nonce="${MEWORK_TERMINAL_CONTROL_NONCE-}"
__mework_channel="${MEWORK_TERMINAL_CONTROL_CHANNEL-}"
builtin unset MEWORK_TERMINAL_CONTROL_NONCE MEWORK_TERMINAL_CONTROL_CHANNEL
__mework_generation=0
__mework_active=0
__mework_pending=0
__mework_opened=0
__mework_editor_defaulted=0
# bash before 4.4 moves to a new line before a `bind -x` command runs and then
# draws the whole prompt and line again below; later versions clear the line.
__mework_old_readline=0
if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
  __mework_old_readline=1
fi
# Private keys, all behind C-x followed by a control byte nothing binds.
__mework_key_bol='\C-x\034'
__mework_key_accept='\C-x\035'
__mework_key_commit='\C-x\036'
__mework_key_next='\C-x\037'
__mework_key_edit='\C-x\032'
__mework_key_close='\C-x\031'

__mework_control() {
  builtin printf '\033]633;Mework;v1;%s;%s;%s\007' "$__mework_nonce" "$1" "$2"
}

"#;

/// Waits for the answer to generation `$1`: 0 accepted, 1 refused, 2 none.
const FIFO_WAIT: &str = r#"__mework_await_decision() {
  local reply
  while IFS= builtin read -r -t @DECISION_TIMEOUT@ reply < "$__mework_channel"; do
    case $reply in
      "a $1") return 0 ;;
      "r $1") return 1 ;;
    esac
  done
  return 2
}

__mework_prepare_decisions() {
  :
}

"#;

/// The reply-file wait polls, because nothing on the MSYS side can block on a
/// file appearing. A poll sleeps with `read -t` on a FIFO the shell opens
/// read-write, which never has data, so it costs no process; a bash too old for
/// a fractional timeout, or a FIFO that turns out not to block, falls back to
/// `sleep`. The deadline is counted in `SECONDS`, not in polls, because a poll
/// that spawns `sleep` takes far longer than it asks for on Windows.
const REPLY_FILE_WAIT: &str = r#"__mework_await_decision() {
  local reply file="$__mework_channel/reply-$1" deadline=$((SECONDS + @DECISION_TIMEOUT@))
  while (( SECONDS < deadline )); do
    if [[ -f $file ]]; then
      reply=''
      IFS= builtin read -r reply < "$file" || :
      case $reply in
        a) return 0 ;;
        r) return 1 ;;
      esac
    fi
    __mework_nap
  done
  return 2
}

__mework_quiet=''
__mework_nap() {
  local ignored status=0
  if [[ -n $__mework_quiet ]]; then
    IFS= builtin read -r -t 0.02 ignored <> "$__mework_quiet" || status=$?
    if (( status > 128 )); then
      return 0
    fi
    __mework_quiet=''
  fi
  command sleep 0.02 2>/dev/null || command sleep 1 || :
}

__mework_prepare_decisions() {
  if (( BASH_VERSINFO[0] >= 4 )) && command mkfifo "$__mework_channel/quiet" 2>/dev/null; then
    __mework_quiet="$__mework_channel/quiet"
  fi
}

"#;

/// Everything below runs inside readline or `PROMPT_COMMAND`, where `set -e`
/// in a user's profile would end the shell on the first failing command: every
/// function returns 0 and keeps its failures inside conditions.
const HOOKS: &str = r#"# Announces generation N+1 and waits for the host. A wait that was cut short
# (Ctrl-C) may still have been granted, so its generation is closed first.
__mework_ask() {
  local decision=0
  if (( __mework_pending )); then
    __mework_control end "$__mework_pending"
    __mework_pending=0
  fi
  __mework_generation=$((__mework_generation + 1))
  __mework_pending=$__mework_generation
  __mework_control start "$__mework_generation"
  __mework_await_decision "$__mework_generation" || decision=$?
  __mework_pending=0
  case $decision in
    0)
      __mework_active=$__mework_generation
      return 0
      ;;
    1) return 1 ;;
  esac
  # No answer: whatever the host decided, this line is not going to run.
  __mework_control end "$__mework_generation"
  return 1
}

# The key after the gate in every macro does what the gate decided.
__mework_commit_with() {
  builtin bind "\"$__mework_key_commit\": $1"
}

# Old readline has already moved to a new line and is about to draw the prompt
# and line again. Moving back to where the prompt began makes that redraw land
# on the original, so a line is not shown twice. The prompt's height is worked
# out from PS1 the cheap way: the escapes that name the user, host and
# directory are expanded, and whatever cannot be measured without running it
# (`$(...)`, `$VAR`, other escapes) counts as nothing. Too few rows leaves a
# stray copy of a prompt line; too many would draw over the output above.
__mework_prompt_rows() {
  local text="${PS1-}" columns="${COLUMNS:-80}" rows=0 width=0 hidden=0 stopped=0
  local skip='' i=0 c add digits dir="${PWD-}"
  if [[ -n "${HOME-}" && ( $dir == "$HOME" || $dir == "$HOME"/* ) ]]; then
    dir="~${dir#"$HOME"}"
  fi
  if (( columns < 1 )); then
    columns=80
  fi
  while (( i < ${#text} )); do
    c="${text:i:1}"
    i=$((i + 1))
    if [[ -n $skip ]]; then
      case $skip$c in
        csi[A-Za-z] | brace}) skip='' ;;
      esac
      continue
    fi
    add=0
    if [[ $c == '\' ]]; then
      c="${text:i:1}"
      i=$((i + 1))
      case $c in
        '[') hidden=1 ;;
        ']') hidden=0 ;;
        n)
          rows=$((rows + (width == 0 ? 1 : (width + columns - 1) / columns)))
          width=0
          stopped=0
          ;;
        e) skip=csi ;;
        [0-7])
          digits=$c
          while [[ ${#digits} -lt 3 && ${text:i:1} == [0-7] ]]; do
            digits="$digits${text:i:1}"
            i=$((i + 1))
          done
          case $((8#$digits)) in
            1) hidden=1 ;;
            2) hidden=0 ;;
            27) skip=csi ;;
          esac
          ;;
        D) skip=brace ;;
        w) add=${#dir} ;;
        W)
          c="${dir##*/}"
          add=$(( ${#c} ? ${#c} : 1 ))
          ;;
        u)
          c="${USER-}"
          add=${#c}
          ;;
        h)
          c="${HOSTNAME%%.*}"
          add=${#c}
          ;;
        H) add=${#HOSTNAME} ;;
        s) add=4 ;;
        '$' | '\') add=1 ;;
      esac
    elif [[ $c == $'\033' ]]; then
      skip=csi
    elif [[ $c == $'\001' ]]; then
      hidden=1
    elif [[ $c == $'\002' ]]; then
      hidden=0
    elif [[ $c == '$' ]]; then
      if (( ! hidden )); then
        stopped=1
      fi
    else
      add=1
    fi
    if (( ! hidden && ! stopped )); then
      width=$((width + add))
    fi
  done
  __mework_rows=$((rows + width / columns))
}

__mework_rewind() {
  builtin printf '\033[%dA\r' "$((__mework_rows + 1))"
}

# bash before 4.4 also forgets, once a `bind -x` command has run, that the next
# line it reads continues this one, and prompts for it with PS1. While a
# command is being entered PS1 therefore shows what PS2 would have; a PS1 the
# command itself sets is left alone.
__mework_swap_prompt() {
  if [[ -z "${__mework_saved_ps1+set}" ]]; then
    __mework_saved_ps1="${PS1-}"
    PS1="${PS2-}"
    __mework_swapped_ps1="$PS1"
  fi
}

__mework_restore_prompt() {
  if [[ -n "${__mework_saved_ps1+set}" ]]; then
    if [[ "${PS1-}" == "$__mework_swapped_ps1" ]]; then
      PS1="$__mework_saved_ps1"
    fi
    unset __mework_saved_ps1 __mework_swapped_ps1
  fi
  return 0
}

# A blank line runs nothing, so it is not announced. bash 3.2 cannot show a
# `bind -x` command the line, so there every line is. Editing in $EDITOR runs
# whatever comes back, however the line started.
__mework_runs_nothing() {
  [[ $1 != edit-and-execute-command ]] && (( BASH_VERSINFO[0] >= 4 )) &&
    [[ -z "${READLINE_LINE//[[:space:]]/}" ]]
}

# vi mode's `v` runs the same command as emacs mode's C-x C-e, whose editor
# falls back to emacs; in vi mode it should fall back to vi.
__mework_default_editor() {
  if [[ $1 == edit-and-execute-command && -z "${VISUAL-}${EDITOR-}" && :$SHELLOPTS: == *:vi:* ]]; then
    VISUAL=vi
    __mework_editor_defaulted=1
  fi
}

__mework_restore_editor() {
  if (( __mework_editor_defaulted )); then
    unset VISUAL
    __mework_editor_defaulted=0
  fi
  return 0
}

# The gate. $1 is the readline command that runs the line. A line that runs
# something is announced and held until the host answers; continuation lines
# of a command already started pass straight through, and a refused line
# stays in the buffer so pressing Enter again retries it.
__mework_gate() {
  if (( __mework_old_readline )); then
    __mework_prompt_rows
  fi
  __mework_hook_prompt
  if (( __mework_active )) || __mework_runs_nothing "$1"; then
    :
  elif __mework_ask; then
    __mework_opened=1
    __mework_default_editor "$1"
    if (( __mework_old_readline )); then
      __mework_swap_prompt
    fi
  else
    builtin printf '%s\n' '@REFUSED_MESSAGE@' >&2
    if (( __mework_old_readline )); then
      __mework_commit_with end-of-line
    else
      __mework_commit_with '""'
    fi
    return 0
  fi
  if (( __mework_old_readline )); then
    __mework_rewind
  fi
  __mework_commit_with "$1"
  return 0
}

# Runs after edit-and-execute-command, whose commands run inside readline
# rather than after it, so no prompt follows to close them.
__mework_close() {
  if (( __mework_opened && __mework_active )); then
    __mework_control end "$__mework_active"
    __mework_active=0
  fi
  __mework_opened=0
  __mework_restore_editor
  if (( __mework_old_readline )); then
    __mework_restore_prompt
    __mework_prompt_rows
    __mework_rewind
  fi
  return 0
}

# Every prompt closes the command the last accepted line started, or says the
# shell is idle. The host ignores all but the first `ready`. bash restores `$?`
# for PS1 by itself after PROMPT_COMMAND.
__mework_prompt() {
  if (( __mework_pending )); then
    __mework_control end "$__mework_pending"
    __mework_pending=0
  fi
  if (( __mework_active )); then
    __mework_control end "$__mework_active"
    __mework_active=0
  else
    __mework_control ready 0
  fi
  __mework_opened=0
  __mework_restore_editor
  __mework_restore_prompt
  return 0
}

# Appends the prompt hook, last, so the user's own PROMPT_COMMAND still runs
# under the command's lease. It is checked again at every gate, because
# re-sourcing a profile commonly assigns PROMPT_COMMAND afresh.
__mework_hook_prompt() {
  if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    local entry
    for entry in ${PROMPT_COMMAND[@]+"${PROMPT_COMMAND[@]}"}; do
      if [[ $entry == __mework_prompt ]]; then
        return 0
      fi
    done
    PROMPT_COMMAND+=(__mework_prompt)
    return 0
  fi
  case $'\n'"${PROMPT_COMMAND-}"$'\n' in
    *$'\n__mework_prompt\n'*) return 0 ;;
  esac
  if (( __mework_old_readline )); then
    PROMPT_COMMAND=$'__mework_restore_prompt\n'"${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}__mework_prompt"
  else
    PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}__mework_prompt"
  fi
  return 0
}

# Binds $2 in keymap $1 to: [beginning-of-line] gate commit [close]. Old
# readline starts from the beginning of the line so the gate knows which screen
# row the cursor is on.
__mework_wrap_key() {
  local macro="$3$__mework_key_commit${4-}"
  if (( __mework_old_readline )); then
    macro="$__mework_key_bol$macro"
  fi
  builtin bind -m "$1" "$2: \"$macro\""
}

__mework_install() {
  local map line bindings v_listed=0
  for map in emacs vi-insert vi-command; do
    builtin bind -m "$map" "\"$__mework_key_bol\": beginning-of-line"
    builtin bind -m "$map" -x "\"$__mework_key_accept\": \"__mework_gate accept-line\""
    builtin bind -m "$map" -x "\"$__mework_key_next\": \"__mework_gate operate-and-get-next\""
    builtin bind -m "$map" -x "\"$__mework_key_edit\": \"__mework_gate edit-and-execute-command\""
    builtin bind -m "$map" -x "\"$__mework_key_close\": \"__mework_close\""
  done
  # Every key the user's files left on a command that runs the line, in every
  # keymap, read before the commit key exists so it is never wrapped itself.
  bindings="$(for map in emacs vi-insert vi-command; do
    builtin printf '#map %s\n' "$map"
    builtin bind -m "$map" -p
  done 2>/dev/null)"
  while IFS= builtin read -r line; do
    case $line in
      '#map '*)
        map="${line#'#map '}"
        continue
        ;;
      '"v": '*)
        if [[ $map == vi-command ]]; then
          v_listed=1
        fi
        ;;
    esac
    case $line in
      *': accept-line')
        __mework_wrap_key "$map" "${line%': '*}" "$__mework_key_accept"
        ;;
      *': operate-and-get-next')
        __mework_wrap_key "$map" "${line%': '*}" "$__mework_key_next"
        ;;
      *': edit-and-execute-command' | *': vi-edit-and-execute-command')
        __mework_wrap_key "$map" "${line%': '*}" "$__mework_key_edit" "$__mework_key_close"
        ;;
    esac
  done <<< "$bindings"
  # vi mode's `v` edits and runs the line through a command bash 3.2 gives no
  # name, so it is not listed; unless the user bound it to something else, it
  # goes through the gate with the named equivalent.
  if (( ! v_listed )); then
    __mework_wrap_key vi-command '"v"' "$__mework_key_edit" "$__mework_key_close"
  fi
  for map in emacs vi-insert vi-command; do
    builtin bind -m "$map" "\"$__mework_key_commit\": accept-line"
  done
  __mework_prepare_decisions
  __mework_hook_prompt
  return 0
}

"#;

/// Login-shell startup, as `bash -l` runs it, at top level: a file sourced
/// inside a function would turn every bare `declare` in it into a local.
const STARTUP: &str = r#"if [ -r /etc/profile ]; then
  builtin source /etc/profile
fi
if [ -r ~/.bash_profile ]; then
  builtin source ~/.bash_profile
elif [ -r ~/.bash_login ]; then
  builtin source ~/.bash_login
elif [ -r ~/.profile ]; then
  builtin source ~/.profile
fi

__mework_install
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_leave_the_environment_before_any_user_file_runs() {
        for decision in [Decision::Fifo, Decision::ReplyFiles] {
            let rc = bashrc(decision);
            let unset = rc
                .find("builtin unset MEWORK_TERMINAL_CONTROL_NONCE MEWORK_TERMINAL_CONTROL_CHANNEL")
                .unwrap();
            let first_source = rc.find("builtin source").unwrap();
            assert!(unset < first_source);
            assert!(rc.find("__mework_nonce=").unwrap() < unset);
        }
    }

    #[test]
    fn user_files_are_sourced_at_top_level_in_login_order_and_hooks_install_last() {
        let rc = bashrc(Decision::Fifo);
        let order = [
            "builtin source /etc/profile",
            "builtin source ~/.bash_profile",
            "builtin source ~/.bash_login",
            "builtin source ~/.profile",
        ]
        .map(|needle| rc.find(needle).unwrap_or_else(|| panic!("{needle}")));
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
        // Every function is defined before the user's files, and the only
        // thing after them is the call that installs the hooks.
        let last_definition = rc.rfind("() {").unwrap();
        assert!(last_definition < order[0]);
        let tail = &rc[order[3]..];
        assert_eq!(tail.matches("__mework_install").count(), 1);
        assert!(rc.trim_end().ends_with("__mework_install"));
        // The sourcing lines are not inside a function body.
        let startup = &rc[rc.find("if [ -r /etc/profile ]").unwrap()..];
        assert!(!startup.contains("() {"));
    }

    #[test]
    fn placeholders_are_filled_and_each_route_waits_its_own_way() {
        let fifo = bashrc(Decision::Fifo);
        let files = bashrc(Decision::ReplyFiles);
        for rc in [&fifo, &files] {
            assert!(!rc.contains("@DECISION_TIMEOUT@") && !rc.contains("@REFUSED_MESSAGE@"));
            assert!(rc.contains(REFUSED_MESSAGE));
        }
        assert!(fifo.contains(r#"builtin read -r -t 30 reply < "$__mework_channel""#));
        assert!(!fifo.contains("reply-$1"));
        assert!(files.contains(r#"file="$__mework_channel/reply-$1""#));
        assert!(files.contains("SECONDS + 30"));
        assert!(!files.contains(r#"< "$__mework_channel";"#));
    }
}
