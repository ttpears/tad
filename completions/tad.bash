function _tad_complete() {
   local cur prev
   cur="${COMP_WORDS[COMP_CWORD]}"
   prev="${COMP_WORDS[COMP_CWORD-1]}"

   # All known hosts across all groups — used to suggest hosts when adding a
   # new group, since tad doesn't otherwise track a host inventory.
   _tad_all_hosts() {
      { local g
        for g in $(tad groups list 2>/dev/null | cut -d: -f1); do
           tad groups hosts "$g" 2>/dev/null
        done
        tad complete-hosts 2>/dev/null | cut -f1
      } | sort -u
   }

   # ---- top-level (position 1) ----
   if (( COMP_CWORD == 1 )); then
      local subs="groups config status tmux-keybind watch -g"
      local sessions hosts
      sessions=$(tad complete 2>/dev/null | cut -f2 | cut -d: -f1)
      hosts=$(tad complete-hosts 2>/dev/null | cut -f1)
      COMPREPLY=( $(compgen -W "$subs $sessions $hosts" -- "$cur") )
      compopt -o nosort 2>/dev/null
      return
   fi

   # tad -g <group> [<host>]
   if [[ ${COMP_WORDS[1]} == -g ]]; then
      if (( COMP_CWORD == 2 )); then
         COMPREPLY=( $(compgen -W "$(tad groups list 2>/dev/null | cut -d: -f1)" -- "$cur") )
      elif (( COMP_CWORD == 3 )); then
         COMPREPLY=( $(compgen -W "$(tad groups hosts "${COMP_WORDS[2]}" 2>/dev/null)" -- "$cur") )
      fi
      return
   fi

   # ---- tad groups <sub> [args] ----
   if [[ ${COMP_WORDS[1]} == groups ]]; then
      # position 2 = subcommand
      if (( COMP_CWORD == 2 )); then
         COMPREPLY=( $(compgen -W "list hosts add rm edit" -- "$cur") )
         return
      fi
      case ${COMP_WORDS[2]} in
         hosts|rm)
            if (( COMP_CWORD == 3 )); then
               COMPREPLY=( $(compgen -W "$(tad groups list 2>/dev/null | cut -d: -f1)" -- "$cur") )
            elif (( COMP_CWORD == 4 )) && [[ ${COMP_WORDS[2]} == hosts ]]; then
               # Informational: show hosts in the chosen group
               COMPREPLY=( $(compgen -W "$(tad groups hosts "${COMP_WORDS[3]}" 2>/dev/null)" -- "$cur") )
            fi
            return
            ;;
         add)
            # tad groups add <name> <layout> <host>...
            # position 3 = NAME (free-form, no completion)
            if (( COMP_CWORD == 4 )); then
               COMPREPLY=( $(compgen -W "panes synced-panes windows browse" -- "$cur") )
               return
            fi
            if (( COMP_CWORD >= 5 )); then
               COMPREPLY=( $(compgen -W "$(_tad_all_hosts)" -- "$cur") )
               return
            fi
            ;;
      esac
   fi
}

complete -F _tad_complete tad
