function _tad_complete() {
   local cur prev opts
   cur="${COMP_WORDS[COMP_CWORD]}"
   prev="${COMP_WORDS[COMP_CWORD-1]}"

   # First arg: subcommands + session names + -g
   if (( COMP_CWORD == 1 )); then
      local subs="complete groups group-hosts groups-add groups-rm groups-edit -g"
      local sessions
      sessions=$(tad complete 2>/dev/null | cut -f2 | cut -d: -f1)
      COMPREPLY=( $(compgen -W "$subs $sessions" -- "$cur") )
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
}

complete -F _tad_complete tad
