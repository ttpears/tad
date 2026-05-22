function _tad_complete() {
   local cur prev opts
   cur="${COMP_WORDS[COMP_CWORD]}"
   prev="${COMP_WORDS[COMP_CWORD-1]}"

   # All known hosts across all groups — used to suggest hosts when adding a
   # new group, since tad doesn't otherwise track a host inventory.
   _tad_all_hosts() {
      local g
      for g in $(tad groups 2>/dev/null | cut -d: -f1); do
         tad group-hosts "$g" 2>/dev/null
      done | sort -u
   }

   # First arg: subcommands + session names + group names + -g
   if (( COMP_CWORD == 1 )); then
      local subs="complete groups group-hosts groups-add groups-rm groups-edit config tmux-keybind status -g"
      local sessions groups
      sessions=$(tad complete 2>/dev/null | cut -f2 | cut -d: -f1)
      groups=$(tad groups 2>/dev/null | cut -d: -f1)
      COMPREPLY=( $(compgen -W "$subs $sessions $groups" -- "$cur") )
      compopt -o nosort 2>/dev/null
      return
   fi

   # tad -g <group> [<host>]
   if [[ ${COMP_WORDS[1]} == -g ]]; then
      if (( COMP_CWORD == 2 )); then
         opts=$(tad groups 2>/dev/null | cut -d: -f1)
         COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
      elif (( COMP_CWORD == 3 )); then
         opts=$(tad group-hosts "${COMP_WORDS[2]}" 2>/dev/null)
         COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
      fi
      return
   fi

   # tad group-hosts <group> | tad groups-rm <group>
   if [[ $prev == group-hosts || $prev == groups-rm ]]; then
      opts=$(tad groups 2>/dev/null | cut -d: -f1)
      COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
      return
   fi

   # tad group-hosts <group> <TAB> → show hosts informationally
   if (( COMP_CWORD >= 3 )) && [[ ${COMP_WORDS[COMP_CWORD-2]} == group-hosts ]]; then
      opts=$(tad group-hosts "${COMP_WORDS[COMP_CWORD-1]}" 2>/dev/null)
      COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
      return
   fi

   # tad groups-rm <group> <TAB> → no more args
   if (( COMP_CWORD >= 3 )) && [[ ${COMP_WORDS[COMP_CWORD-2]} == groups-rm ]]; then
      return
   fi

   # tad groups-add <name> <layout> <host>...
   if [[ ${COMP_WORDS[1]} == groups-add ]]; then
      # position 2 = NAME (free-form, no completion)
      if (( COMP_CWORD == 3 )); then
         COMPREPLY=( $(compgen -W "panes synced-panes windows browse" -- "$cur") )
         return
      fi
      if (( COMP_CWORD >= 4 )); then
         COMPREPLY=( $(compgen -W "$(_tad_all_hosts)" -- "$cur") )
         return
      fi
   fi
}

complete -F _tad_complete tad
