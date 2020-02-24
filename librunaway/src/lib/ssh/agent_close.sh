rw_close(){
    printf 'RUNAWAY_CWD: %s\n' "$(pwd)"
    env | sed 's/^/RUNAWAY_ENV: /g'
    rm "$stdout_fifo"
    rm "$stderr_fifo"
    echo 'RUNAWAY_EOF:'
}
